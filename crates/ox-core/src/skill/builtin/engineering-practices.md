---
name: engineering-practices
description: Universal engineering practices for software development. Covers file organization, documentation standards, code conventions, and development workflows applicable to any project.
scope: global
---

# Engineering Practices (Universal)

## When to Use
- Starting a new project or feature
- Organizing code and documentation
- Following team conventions
- Understanding best practices for software development

## Core Principles

### 0. Plan Before Execute (MANDATORY)

**BEFORE implementing ANY non-trivial task, you MUST:**

1. **Analyze the request** - Understand what needs to be done
2. **Create a step-by-step plan** - Break down into clear, verifiable steps
3. **Present the plan to user** - Explain your approach and ask for confirmation
4. **Wait for approval** - Do NOT start coding until user confirms
5. **Execute step by step** - Follow the approved plan
6. **Verify each step** - Ensure each step works before moving to next

**Example:**
```
User: "Add user authentication to the API"

Assistant: "Here's my plan:

Step 1: Add JWT dependency to Cargo.toml
Step 2: Create auth module with login/register endpoints
Step 3: Add middleware for token validation
Step 4: Write integration tests
Step 5: Update API documentation

Does this plan look good? Please confirm or suggest changes."

[Wait for user confirmation]

Assistant: "Starting Step 1: Adding JWT dependency..."
```

**When this applies:**
- Adding new features
- Refactoring existing code
- Fixing complex bugs
- Any task requiring multiple file changes
- Architectural decisions

**When this does NOT apply:**
- Simple one-line fixes
- Answering questions
- Reading files
- Running read-only commands

---

### 1. File Organization

**Standard Project Structure:**
```
project/
├── src/                  # Source code
│   ├── main.rs           # Entry point (if applicable)
│   ├── lib.rs            # Library root (if applicable)
│   ├── modules/          # Feature modules
│   └── utils/            # Utility functions
├── tests/                # Integration tests
├── docs/                 # Documentation
├── config/               # Configuration files
├── scripts/              # Build/deployment scripts
└── README.md             # Project overview
```

**Rules:**
- Group related code together (high cohesion)
- Separate concerns into different modules/files
- Keep directory structure flat when possible
- Use consistent naming across the project

### 2. Documentation Standards

**Location:** All docs go in `docs/` directory or alongside code

**Naming:**
- Use kebab-case: `api-reference.md`, `setup-guide.md`
- Be descriptive: `database-migration-strategy.md` (not `doc1.md`)
- Prefix with numbers for ordered guides: `01-setup.md`, `02-usage.md`

**Format:**
- Markdown for all text documentation
- Include code examples where relevant
- Add diagrams (Mermaid/PlantUML) for complex flows
- Link related documents

**Example Structure:**
```markdown
# Feature Name

## Overview
Brief description of what this feature does.

## Usage
How to use this feature with examples.

## Implementation
Key implementation details and design decisions.

## Related
- [Related Doc](./related-doc.md)
- [API Reference](../api/reference.md)
```

### 3. Code Conventions

**General Style:**
- Follow language-specific style guides (e.g., PEP 8 for Python, rustfmt for Rust)
- Use consistent naming conventions:
  - `snake_case` for functions/variables (most languages)
  - `PascalCase` for types/classes
  - `SCREAMING_SNAKE_CASE` for constants
- Indentation: 2 or 4 spaces (consistent within project)
- Line length: 80-120 characters

**Error Handling:**
- Fail fast and explicitly
- Provide meaningful error messages
- Handle edge cases gracefully
- Log errors with context

**Testing:**
- Write tests for all public APIs
- Test both happy path and edge cases
- Use descriptive test names: `test_<function>_<scenario>()`
- Keep tests independent and repeatable

### 4. Development Workflow

**Before Committing:**
1. Run linters/formatters
2. Run all tests
3. Review your own changes
4. Update relevant documentation
5. Ensure commit message is clear

**Commit Messages:**
- Format: `<type>: <description>`
- Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`
- Example: `feat: add user authentication with JWT`
- Keep first line under 72 characters
- Add detailed description if needed

**Branch Naming:**
- Feature: `feat/user-authentication`
- Bugfix: `fix/login-error-handling`
- Refactor: `refactor/database-layer`
- Hotfix: `hotfix/critical-security-issue`

### 5. Code Review Guidelines

**What to Check:**
- Correctness: Does it work as intended?
- Readability: Is it easy to understand?
- Performance: Are there obvious bottlenecks?
- Security: Any vulnerabilities?
- Testing: Are edge cases covered?
- Documentation: Is it documented?

**Review Etiquette:**
- Be constructive and specific
- Explain why, not just what
- Suggest alternatives when possible
- Acknowledge good solutions

## Anti-patterns

❌ **DON'T:**
- Start coding without presenting a plan first
- Put documentation outside standard locations
- Mix business logic with UI/presentation code
- Skip tests when adding features
- Use magic numbers without explanation
- Hardcode configuration values
- Ignore compiler/linter warnings
- Create overly complex abstractions for simple problems
- Duplicate code instead of extracting functions
- Write long functions (>50 lines) without justification
- Commit without running tests
- Assume you know what the user wants without asking

✅ **DO:**
- Present a clear plan before starting implementation
- Keep modules focused and cohesive
- Write self-documenting code with clear names
- Add comments for "why", not "what"
- Use existing patterns before creating new ones
- Update docs when changing behavior
- Ask clarifying questions before implementing
- Refactor when you see duplication
- Write small, testable functions
- Follow DRY (Don't Repeat Yourself) principle
- Prefer composition over inheritance

## Examples

### Example 1: Good Function Design

```python
# Bad: Too many responsibilities
def process_data(data):
    # validation
    # transformation
    # database save
    # email notification
    pass

# Good: Single responsibility
def validate_input(data: dict) -> bool:
    """Validate input data format."""
    return 'email' in data and '@' in data['email']

def transform_data(data: dict) -> dict:
    """Transform data to internal format."""
    return {'user_email': data['email'].lower()}

def save_to_database(data: dict) -> None:
    """Save transformed data to database."""
    db.insert('users', data)
```

### Example 2: Error Handling

```rust
// Bad: Silent failure
fn read_config(path: &str) -> Config {
    let content = std::fs::read_to_string(path).unwrap(); // Panics!
    serde_json::from_str(&content).unwrap()
}

// Good: Explicit error handling
#[derive(Debug)]
enum ConfigError {
    IoError(std::io::Error),
    ParseError(serde_json::Error),
}

fn read_config(path: &str) -> Result<Config, ConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(ConfigError::IoError)?;
    
    serde_json::from_str(&content)
        .map_err(ConfigError::ParseError)
}
```

### Example 3: Test Writing

```python
# Good test structure
class TestUserAuthentication:
    def test_login_with_valid_credentials(self):
        user = authenticate("user@example.com", "password123")
        assert user is not None
        assert user.email == "user@example.com"
    
    def test_login_with_invalid_password(self):
        with pytest.raises(AuthenticationError):
            authenticate("user@example.com", "wrong_password")
    
    def test_login_with_nonexistent_user(self):
        with pytest.raises(UserNotFoundError):
            authenticate("nonexistent@example.com", "password")
```

## Best Practices Summary

### Code Quality
- **Readability > Cleverness**: Write code for humans, not machines
- **Simplicity First**: The simplest solution that works is usually best
- **Consistency**: Follow established patterns in the codebase
- **Documentation**: Document intent, not implementation

### Collaboration
- **Communication**: Discuss design decisions before implementing
- **Feedback**: Give and receive code review feedback constructively
- **Knowledge Sharing**: Document learnings and share with team
- **Continuous Improvement**: Refactor when you see opportunities

### Maintenance
- **Technical Debt**: Address it early, don't let it accumulate
- **Dependencies**: Keep them updated and minimal
- **Backwards Compatibility**: Consider impact of breaking changes
- **Deprecation**: Mark deprecated APIs clearly with migration paths