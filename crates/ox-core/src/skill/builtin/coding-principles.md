---
name: coding-principles
description: Four core principles for disciplined coding: think before coding, simplicity first, surgical changes, and goal-driven execution.
scope: global
---

# Coding Principles (4 Core Rules)

Apply these principles in ALL code you write.

## 1. Think Before Coding

**When in doubt, ASK. Never assume or guess.**

### When to Use
- Request is unclear or has multiple valid interpretations
- You're uncertain about user's intent
- Multiple approaches exist with different trade-offs
- Requirements seem incomplete or contradictory

### Steps
1. Identify the ambiguity or uncertainty
2. Formulate a clear clarifying question
3. Present options if applicable (with pros/cons)
4. Wait for user confirmation before proceeding
5. Only start coding after receiving clear direction

### Anti-patterns
- Making assumptions without verification
- Proceeding when confused
- Guessing user preferences
- Starting implementation before understanding the full scope
- Choosing one interpretation without asking

### Example
**Bad:**
User: "Add validation"
Assistant: [Immediately implements email validation]

**Good:**
User: "Add validation"
Assistant: "What kind of validation do you need?
- Input format validation (email, phone, etc.)?
- Business logic validation (age range, permissions)?
- Database constraint validation?

Please clarify which type(s) you want."

---

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

### When to Use
- Writing new features or functions
- Implementing user requests
- Refactoring existing code
- Adding error handling or edge cases

### Steps
1. Understand the exact requirement (no more, no less)
2. Implement the simplest solution that works
3. Avoid adding "just in case" features
4. Resist the urge to abstract prematurely
5. If it could be simpler, simplify it further

### Anti-patterns
- Adding features beyond what was asked
- Creating abstractions for single-use code
- Adding "flexibility" or "configurability" unless requested
- Over-engineering with design patterns when not needed
- Building for hypothetical future requirements

### Example
**Bad (Over-engineered):**
```rust
// User asked for a simple counter, but you created a complex abstraction
trait Counter {
    fn increment(&mut self);
    fn decrement(&mut self);
    fn reset(&mut self);
    fn get_value(&self) -> i32;
}

struct AdvancedCounter {
    value: i32,
    history: Vec<i32>,
    listeners: Vec<Box<dyn Fn(i32)>>,
}
```

**Good (Simple):**
```rust
fn increment_counter(count: &mut u32) {
    *count += 1;
}
```

---

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

### When to Use
- Modifying existing files
- Fixing bugs in established codebases
- Adding small features to existing modules
- Refactoring specific sections of code

### Steps
1. Read the file first to understand current state
2. Identify the exact lines/sections that need change
3. Make only the necessary modifications
4. Remove imports/variables/functions YOUR changes made unused
5. Match existing code style and conventions
6. Leave pre-existing code untouched (even if imperfect)

### Anti-patterns
- Improving adjacent code that isn't related to your task
- Changing formatting or style of unrelated code
- Refactoring things that aren't broken
- Removing pre-existing dead code unless explicitly asked
- "While I'm here, let me fix this too" mentality

### Example
**Bad:**
```rust
// User asked to add one line
fn process_data(data: Vec<String>) -> Result<()> {
    // Old messy code with inconsistent spacing
    let result=data.iter()
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>();
    
    // Your change
    println!("Processing {} items", result.len());
    
    // You also "fixed" the formatting above - DON'T!
}
```

**Good:**
```rust
// Only add what's needed, leave everything else as-is
fn process_data(data: Vec<String>) -> Result<()> {
    // Old messy code with inconsistent spacing
    let result=data.iter()
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>();
    
    // Your change only
    println!("Processing {} items", result.len());
}
```

---

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

### When to Use
- Starting any non-trivial task
- Implementing features with multiple steps
- Fixing complex bugs
- Refactoring large sections of code
- Any task where "done" is ambiguous

### Steps
1. Define clear success criteria before starting
2. Break the task into verifiable steps
3. For each step, define how to verify completion
4. Execute step by step
5. Verify each step before moving to the next
6. Confirm overall success against original criteria

### Anti-patterns
- Starting without knowing what "done" looks like
- Assuming something works without testing
- Moving to the next step before verifying the current one
- Vague success criteria ("make it better", "optimize it")
- Not having a verification method for each step

### Example
**Bad:**
User: "Add validation"
Assistant: [Writes validation code]
Assistant: "Done!"
[No verification, might not work correctly]

**Good:**
User: "Add validation"
Assistant: "Plan:
1. Write tests for invalid inputs -> verify: tests fail
2. Implement validation logic -> verify: tests pass
3. Test edge cases -> verify: all cases handled

Step 1: Writing tests...
[writes tests]
Running tests... ✅ Tests fail as expected (validation not implemented yet)

Step 2: Implementing validation...
[writes code]
Running tests... ✅ All tests pass

Step 3: Testing edge cases...
[test edge cases]
✅ All edge cases handled

Task complete! Validation is working correctly."
