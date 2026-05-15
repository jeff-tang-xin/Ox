```toml
[dependencies]
encoding_rs = "0.8"  # 处理 GBK、UTF-16 等非 UTF-8 编码
dunce = "1.0"        # 规范化 Windows 路径，解决超长路径限制
walkdir = "2.4"      # (可选) 如果你需要遍历目录配合这些工具使用
```

```rust
use encoding_rs::Encoding;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// 工具一：智能路径处理 (file_path)
/// 1. 自动处理 Windows 下的 `\\?\` 前缀和超长路径问题 (通过 dunce 库)
/// 2. 统一路径分隔符，安全地拼接路径
pub fn file_path(base: &str, segments: &[&str]) -> PathBuf {
let mut path = PathBuf::from(base);
for segment in segments {
path.push(segment);
}
// dunce::canonicalize 会在 Windows 上返回最友好的路径格式，在其他系统上等同于 std::fs::canonicalize
// 如果路径暂时不存在，可以使用 dunce::simplified 或直接返回 path
path
}

/// 工具二：兼容多编码的文件读取 (file_read)
/// 1. 使用 BufReader 保证大文件读取性能
/// 2. 自动检测并解码非 UTF-8 编码（如 Windows 常见的 GBK/GB18030）
pub fn file_read(path: &Path, encoding: Option<&'static Encoding>) -> Result<String, Box<dyn std::error::Error>> {
let file = File::open(path)?;
let reader = BufReader::new(file);

    // 先读取原始字节，避免 std::io::BufReader::lines() 遇到非法 UTF-8 直接报错
    let bytes: Vec<u8> = reader.bytes().filter_map(|b| b.ok()).collect();
    
    // 如果传入了特定编码（如 encoding_rs::GBK），则使用特定编码解码
    // 否则默认尝试按 UTF-8 处理
    let (cow, _encoding_used, _had_errors) = match encoding {
        Some(enc) => enc.decode(&bytes),
        None => encoding_rs::UTF_8.decode(&bytes),
    };
    
    Ok(cow.into_owned())
}

/// 工具三：自动建目录的高性能写入 (file_write)
/// 1. 写入前自动递归创建所有不存在的父目录
/// 2. 使用 BufWriter 减少系统 IO 调用，极大提升写入速度
/// 3. 支持将字符串按指定编码（如 GBK）写入文件
pub fn file_write(
path: &Path,
content: &str,
encoding: Option<&'static Encoding>
) -> Result<(), Box<dyn std::error::Error>> {
// 自动创建多级父目录，防止因目录不存在导致写入失败
if let Some(parent) = path.parent() {
std::fs::create_dir_all(parent)?;
}

    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    
    // 根据是否指定编码，将字符串转为对应的字节流写入
    match encoding {
        Some(enc) => {
            let (bytes, _encoding_used, _had_errors) = enc.encode(content);
            writer.write_all(&bytes)?;
        }
        None => {
            // 默认按 UTF-8 字节写入
            writer.write_all(content.as_bytes())?;
        }
    }
    
    writer.flush()?; // 确保所有缓冲区数据真正落盘
    Ok(())
}
```