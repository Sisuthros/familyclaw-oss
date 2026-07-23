# Bug bounty (public program outline)

FamilyClaw does **not** yet run a paid public bug bounty. This page is the
program skeleton so security researchers and enterprise buyers know the
intent and scope.

## In scope (when the program opens)

- `familyclaw-gateway` HTTP surface (auth bypass, injection, SSRF via skills)
- Approval/resume TOCTOU and payload-hash bypasses
- WASM sandbox escape / capability confusion (`wasmtime` feature)
- Layer A/B boundary leaks in the public tree
- At-most-once dispatch violations under crash (duplicate side effects)

## Out of scope

- Social engineering / physical
- DoS against third-party LLM providers
- Issues only present with Layer B secrets the reporter introduced
- Reports without a reproducible PoC against a tagged release

## Safe harbor

Good-faith research that follows this policy and avoids privacy violations /
data destruction will not trigger legal threats from the maintainers.

## Reporting (interim)

Until a paid platform is live, email the contact in
`docs/COMMERCIAL_OFFER.md` with:

1. Affected version / commit  
2. Reproduction steps  
3. Impact  
4. Suggested fix (optional)  

Do not file secrets or private persona material in public GitHub issues.

## Triage SLA (pilot)

| Severity | First response |
|---|---|
| Critical (RCE / auth bypass on default config) | 2 business days |
| High | 5 business days |
| Medium/Low | 10 business days |
