/// Skill 创建提示词模板（用于 /skill create-llm 和自动反思）
pub const SKILL_CREATION_PROMPT: &str = "\
You are creating a Skill document. A Skill is a reusable pattern or best practice that can guide future tasks.

## Skill Format

Each Skill is a Markdown file with YAML frontmatter:

```markdown
---
name: <skill-id>
description: <brief-description>
scope: project | global
---

# <Skill Name>

## When to Use
- Situation 1
- Situation 2

## Steps
1. Step 1
2. Step 2

## Anti-patterns
- What NOT to do

## Example
Code example or detailed scenario
```

## Scope Decision

Choose the appropriate scope for this Skill:

### **project** (Project-level)
Use when the Skill is specific to:
- This project's architecture or design patterns
- Project-specific tech stack combinations
- Business logic unique to this application
- Team conventions or coding standards
- Project-specific tool configurations

Examples:
- JWT authentication pattern for this Rust web app
- Database migration workflow for PostgreSQL + Diesel
- Error handling conventions in this codebase

### **global** (Global/System-level)
Use when the Skill is universally applicable:
- Language-specific best practices (Rust, Python, etc.)
- General design patterns (Singleton, Factory, etc.)
- Cross-project debugging techniques
- Universal testing strategies
- General performance optimization tips

Examples:
- Rust async/await error handling
- Effective Git commit message format
- Debugging memory leaks in any language

**Decision Rule:** When in doubt, prefer **project** scope. Global Skills should be truly universal.

## Rules

1. **Keep Skills focused and concise** (50-100 lines max)
2. **Each Skill should cover ONE specific topic** - don't combine unrelated patterns
3. **Include concrete examples** - abstract advice is not helpful
4. **List anti-patterns to avoid** - what NOT to do is as important as what to do
5. **Only create Skills for genuinely valuable patterns** - not every task needs a Skill

### Critical: Avoid Duplication and Over-Engineering

**Before creating a new Skill, check if a similar one already exists:**
- If there's a similar Skill, UPDATE it instead of creating a new one
- Merge related content into existing Skills when appropriate
- Example: All coding rules or best practices should be in ONE Skill, not split across multiple

**Examples of BAD design:**
- rust-error-handling + rust-result-type + rust-panic-recovery (too fragmented)
- One comprehensive Skill is better than many small ones

**Examples of GOOD design:**
- Separate Skills for different domains: auth-patterns, db-migration, api-design
- Don't create: auth-jwt, auth-session, auth-oauth (merge into auth-patterns)

**Target: Strict Skill Count Limits**
- **Per project**: Maximum 3 Skills (even if the project has many modules)
- **Global/System-wide**: Maximum 5 Skills
- **Total across all projects**: Must stay under 10 Skills

**CRITICAL: Quality Over Quantity**
- If you think you need more than 3 project Skills, YOU ARE DOING IT WRONG
- A well-designed Skill should be broad enough to cover multiple related scenarios
- Low-value or trivial patterns should NOT become Skills
- If 3 Skills cannot cover a project's needs, the Skills are too fragmented - merge them!

**Strategy for Project Skills:**
- Extract COMMON patterns that apply across multiple modules into ONE Skill
- Don't create one Skill per module (e.g., don't create auth-skill, user-skill, order-skill separately)
- Instead, create broad Skills like: project-coding-standards, project-architecture-patterns, project-best-practices
- If you already have 3 project Skills, DO NOT create more - update existing ones instead

**Strategy for Global Skills:**
- Only create truly universal patterns (language features, general algorithms, common design patterns)
- Be extremely selective - global Skills should benefit MANY different projects
- If you already have 5 global Skills, DO NOT create more - your abstractions are too fragmented
- **Global Skills must be HIGHLY COHESIVE**: each should represent a fundamental, reusable capability
- If 5 global Skills cannot cover universal needs, the Skills are poorly designed - merge and abstract better!

**What makes a GOOD Global Skill:**
- Applies across programming languages or frameworks (e.g., clean-code-principles)
- Captures fundamental computer science concepts (e.g., algorithm-optimization-patterns)
- Encodes universal best practices (e.g., security-best-practices, testing-strategies)
- Teaches transferable problem-solving approaches (e.g., debugging-methodology)

**What makes a BAD Global Skill:**
- Language-specific syntax details (should be in project Skills or documentation)
- Framework-specific patterns (belongs to project Skills)
- Narrow use cases that don't generalize
- Overly specific workflows
- **Common knowledge that LLM already knows** (e.g., use Result in Rust, write tests, use Git)
- **Generic best practices** without project-specific context

**Examples of BAD design:**
- Creating separate Skills for each module: auth.md, users.md, orders.md, products.md (4 Skills = too many!)
- Creating Skills for trivial or obvious patterns
- Duplicating content across multiple Skills
- Having more than 3 project Skills (indicates poor design)

**Examples of GOOD design:**
- One comprehensive Skill: project-patterns.md covering auth, users, orders, etc.
- Another Skill: project-testing-strategy.md for all testing conventions
- Third Skill: project-deployment-guide.md for deployment processes
- Total: 3 project Skills (within limit)

**Self-Check Before Creating:**
1. Is this pattern truly valuable and reusable?
2. Can it be merged into an existing Skill?
3. Does it apply to multiple scenarios, not just one case?
4. Would another developer genuinely benefit from this?
5. Am I staying within the 3-Skill limit per project?
6. **Is this something LLM already knows?** (If yes, DON'T create)
7. **Is this project-specific or team-specific knowledge?** (If no, reconsider)

### 🚫 DO NOT Create Skills For:
- Language basics (syntax, standard library usage)
- Common design patterns (Singleton, Factory, Observer, etc.)
- General best practices (write tests, use version control, code review)
- Well-known frameworks' basic usage
- Information easily found in official documentation

### ✅ DO Create Skills For:
- Project-specific architecture decisions
- Team conventions that aren't obvious
- Complex workflows unique to this codebase
- Lessons learned from debugging difficult issues
- Integration patterns between multiple systems

## Your Task

{task_description}

Please create a Skill document following the format above.
Return ONLY the Markdown content with YAML frontmatter, no explanation.";
