---
name: documentation-output
description: Create documentation files in the docs/ directory. Use appropriate formats and naming conventions for project documentation.
scope: project
---

# Documentation Output

## When to Use
- Creating architectural documents
- Writing technical specifications
- Documenting code decisions
- Adding project guides or tutorials
- Recording lessons learned or best practices

## Steps

### 1. Choose the Right Location
- Store all documentation in the `docs/` directory
- Use subdirectories for organization: `docs/api/`, `docs/architecture/`, `docs/guides/`
- Name files descriptively: `api-reference.md`, `architecture-overview.md`, `setup-guide.md`

### 2. Select Appropriate Format
- Use Markdown (`.md`) for most documentation
- Use PDF for formal specifications or diagrams
- Use images (PNG/JPG/SVG) for diagrams and screenshots
- Use code files (`.rs`, `.json`, etc.) for examples

### 3. Follow Naming Conventions
- Use lowercase with hyphens: `user-authentication.md` (not `UserAuthentication.md` or `user_authentication.md`)
- Prefix with numbers for ordered guides: `01-setup.md`, `02-configuration.md`, `03-usage.md`
- Use descriptive names that indicate content: `database-migration-strategy.md` (not `doc1.md`)

### 4. Structure Documentation
- Include a clear title at the top
- Add a brief description/introduction
- Use headings and subheadings for organization
- Include code examples where relevant
- Add diagrams or screenshots when helpful
- Link to related documentation

### 5. Update Index/Navigation
- Add new documents to `docs/README.md` or `docs/index.md`
- Update navigation menus if applicable
- Cross-reference related documents

## Anti-patterns
- Creating documentation outside the `docs/` directory
- Using generic names like `doc1.md`, `notes.txt`
- Mixing documentation formats unnecessarily
- Creating documentation without linking it to existing docs
- Writing overly verbose or unstructured documents
- Placing large binary files in the main docs/ directory without subdirectories

## Examples

### Example 1: API Documentation
```bash
# Create in appropriate subdirectory
docs/api/user-endpoints.md
docs/api/authentication.md
docs/api/error-handling.md
```

### Example 2: Architecture Documentation
```bash
# Organize by system components
docs/architecture/database-layer.md
docs/architecture/api-layer.md
docs/architecture/security-model.md
```

### Example 3: Setup Guides
```bash
# Number for logical sequence
docs/setup/01-environment-setup.md
docs/setup/02-dependency-installation.md
docs/setup/03-configuration.md
```

### Example 4: Good Document Structure
```markdown
# User Authentication Flow

## Overview
Brief description of the authentication system.

## Components
- JWT tokens for session management
- Refresh token rotation
- Role-based access control

## Flow Diagram
![Auth Flow](../assets/auth-flow.png)

## Implementation
```rust
// Example code
pub fn authenticate(credentials: Credentials) -> Result<TokenPair> {
    // implementation
}
```

## Security Considerations
- Tokens expire after 1 hour
- Refresh tokens rotated on each use
- All sensitive data encrypted in transit
```

## Guidelines

### Content Guidelines
- Write for your future self and other developers
- Include both "how" and "why" information
- Document trade-offs and alternatives considered
- Keep examples up-to-date with code changes
- Use consistent terminology throughout

### Organization Guidelines
- Group related documentation together
- Maintain a logical hierarchy
- Use cross-references between related documents
- Regularly review and update outdated documentation
- Archive obsolete documents rather than deleting

### Quality Guidelines
- Proofread for clarity and accuracy
- Test code examples to ensure they work
- Verify all links and references are valid
- Include version information when relevant
- Specify which versions of software/components are covered