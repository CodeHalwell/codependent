# GitHub Operations Integration Pack

This skill package demonstrates a brokered integration pack for GitHub API workflows in Codypendent.

## Key Capabilities

1. **Brokered Secrets**: Declares and reads the `github_token` credential via the Codypendent `SecretBroker`. The secret value is never written to durable storage, logged, or exposed directly to guest code.
2. **Lifecycle Hooks**: Bundles deterministic validation hooks to enforce policy constraints prior to tool execution.
3. **Context-Bound Leases**: Resolves short-lived credential leases that expire automatically and are bound to specific run and capability scopes.

## Usage

Inspect the skill manifest and declared capabilities:
```bash
codypendent skill install examples/integration-pack
```

Declare and bind the required secret:
```bash
codypendent secret declare github_token --backend environment --locator GITHUB_TOKEN --capability github.api.read
```
