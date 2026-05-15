# code_search 

## 依赖
```toml
[dependencies]
grep = "0.3"
grep-cli = "0.1"
grep-regex = "0.1"
grep-searcher = "0.1"
termcolor = "1.4"
crossbeam = "0.8" # 用于高性能无锁并行处理
encoding_rs = "0.8" # 用于处理 GBK 等非 UTF-8 编码

[features]
# 开启 PCRE2 和 SIMD 加速
default = ["grep/pcre2", "grep/simd-accel"]

```
## 代码
```rust
use crossbeam::channel::bounded;
use grep::matcher::{Match, Matcher};
use grep::regex::RegexMatcher;
use grep::searcher::{sinks::UTF8, Searcher, SearcherBuilder};
use grep_cli::{DecompressionReaderBuilder, WalkBuilder};
use std::error::Error;
use std::path::PathBuf;
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
use std::thread;

// 定义结构化的搜索结果
#[derive(Debug, Clone)]
pub struct CodeMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
    pub start: usize,
    pub end: usize,
}

pub struct AdvancedCodeSearch {
    pattern: String,
    path: String,
    case_insensitive: bool,
    use_pcre2: bool,
    search_zip: bool,
    threads: usize,
}

impl AdvancedCodeSearch {
    pub fn new(pattern: String, path: String) -> Self {
        Self {
            pattern,
            path,
            case_insensitive: false,
            use_pcre2: true, // 默认开启强大的 PCRE2 引擎
            search_zip: true, // 默认支持搜索压缩包
            threads: num_cpus::get(), // 自动获取 CPU 核心数
        }
    }

    // 链式调用配置各项高级参数
    pub fn case_insensitive(mut self, yes: bool) -> Self { self.case_insensitive = yes; self }
    pub fn use_pcre2(mut self, yes: bool) -> Self { self.use_pcre2 = yes; self }
    pub fn search_zip(mut self, yes: bool) -> Self { self.search_zip = yes; self }
    pub fn threads(mut self, n: usize) -> Self { self.threads = n; self }

    pub fn run(self) -> Result<Vec<CodeMatch>, Box<dyn Error>> {
        // 1. 构建高度定制化的 Matcher (支持 PCRE2 和智能大小写)
        let matcher = RegexMatcher::builder()
            .case_insensitive(self.case_insensitive)
            .pcre2(self.use_pcre2)
            .build(&self.pattern)?;

        // 2. 构建支持解压和特定编码的 Searcher
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .binary_detection(grep::searcher::BinaryDetection::quit(b'\x00')) // 遇到二进制字符自动跳过
            .build();
        
        // 如果开启压缩包搜索，配置解压读取器
        if self.search_zip {
            searcher.set_preprocessor(
                grep::searcher::Preprocessor::new(DecompressionReaderBuilder::new()),
            );
        }

        // 3. 构建智能 WalkBuilder (完全遵循 .gitignore, .ignore, 跳过隐藏文件)
        let walker = WalkBuilder::new(&self.path)
            .git_ignore(true)
            .hidden(true)
            .build_parallel(); // 启用 ripgrep 底层的并行遍历

        // 4. 使用 crossbeam 通道实现生产者-消费者模型进行并行搜索
        let (tx, rx) = bounded::<CodeMatch>(1000);
        let total_matches = Arc::new(AtomicUsize::new(0));
        let matcher = Arc::new(matcher);
        let searcher = Arc::new(searcher);

        // 生产者：并行遍历文件并提取匹配项
        let tx_clone = tx.clone();
        let total_matches_clone = total_matches.clone();
        walker.run(|| {
            let tx = tx_clone.clone();
            let matcher = matcher.clone();
            let searcher = searcher.clone();
            let total_matches = total_matches_clone.clone();

            Box::new(move |result| {
                let entry = match result {
                    Ok(entry) => entry,
                    Err(_) => return grep_cli::WalkState::Continue,
                };
                if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                    return grep_cli::WalkState::Continue;
                }

                let path = entry.path().to_path_buf();
                let mut local_searcher = Searcher::clone(&searcher);
                
                // 捕获匹配结果的 Sink
                let mut sink = UTF8.with_matcher(&matcher, |mat: Match<'_>| {
                    let line = mat.bytes().to_vec();
                    let line_str = String::from_utf8_lossy(&line).to_string();
                    let code_match = CodeMatch {
                        path: path.to_string_lossy().to_string(),
                        line_number: mat.line_number().unwrap(),
                        line: line_str,
                        start: mat.start(),
                        end: mat.end(),
                    };
                    tx.send(code_match).unwrap();
                    total_matches.fetch_add(1, Ordering::Relaxed);
                    true // 继续搜索该文件的下一处匹配
                });

                let _ = local_searcher.search_path(&matcher, &path, &mut sink);
                grep_cli::WalkState::Continue
            })
        });

        // 关闭发送端，通知消费者生产结束
        drop(tx);

        // 消费者：收集所有匹配结果
        let mut results = Vec::with_capacity(total_matches.load(Ordering::Relaxed));
        for m in rx {
            results.push(m);
        }

        println!("🚀 搜索完成，共找到 {} 处匹配", results.len());
        Ok(results)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // 链式调用，最大化配置你的搜索任务
    let matches = AdvancedCodeSearch::new(r"(?=\w+)fn\s+\w+".to_string(), "./my_project".to_string())
        .case_insensitive(false)
        .use_pcre2(true)       // 开启 PCRE2 支持零宽断言等高级正则
        .search_zip(true)      // 连 .gz / .zip 里的代码都能搜
        .threads(8)            // 指定 8 线程并行
        .run()?;

    // 结果已经是结构化的 Vec，可以直接转 JSON 或做进一步代码分析
    for m in matches.iter().take(5) {
        println!("[{}:{}] {}", m.path, m.line_number, m.line.trim());
    }
    
    Ok(())
}

```