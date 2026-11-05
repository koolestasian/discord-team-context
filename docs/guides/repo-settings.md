# Repository settings checklist

These settings can only be changed by the maintainer in the GitHub web UI. Nothing in the
repository sets them. GitHub renames menu items from time to time; if a path below does
not match, search the settings page for the item name.

Repository: `koolestasian/discord-team-context`. Settings are at
`https://github.com/koolestasian/discord-team-context/settings`.

## Account

- [ ] Turn on two-factor authentication with a passkey or security key. Account
  settings → Password and authentication.
- [ ] Use fine-grained personal access tokens only. Delete unused classic tokens. Account
  settings → Developer settings → Personal access tokens.

## Branch protection (ruleset on `main`)

- [ ] Create a branch ruleset that targets the default branch. Repository settings →
  Rules → Rulesets → New branch ruleset.
- [ ] In that ruleset, enable "Require a pull request before merging".
- [ ] In that ruleset, enable "Require status checks to pass" and select the single
  `ci-ok` check from `ci.yml` (it fails if any CI job fails). Check names only appear
  after CI has run once.
- [ ] In that ruleset, enable "Require linear history".
- [ ] In that ruleset, enable "Block force pushes" and "Restrict deletions".

## Security features

- [ ] Turn on secret scanning and push protection. Repository settings → Advanced
  Security (or Code security) → Secret protection.
- [ ] Turn on Dependabot alerts and Dependabot security updates. Repository settings →
  Advanced Security (or Code security) → Dependabot. Version updates for cargo and
  GitHub Actions need a `.github/dependabot.yml` file, which is not in this change.
- [ ] Choose one CodeQL setup, not both: either the default setup (Code security → Code
  scanning → CodeQL analysis → Default setup) or the `codeql.yml` workflow. Running both
  causes conflicting results.
- [ ] Turn on private vulnerability reporting. Repository settings → Advanced Security (or
  Code security) → Private vulnerability reporting.

## GitHub Actions

- [ ] Set the default `GITHUB_TOKEN` to read-only. Repository settings → Actions →
  General → Workflow permissions → "Read repository contents and packages permissions".
- [ ] Require approval for workflows from first-time contributors. Repository settings →
  Actions → General → Fork pull request workflows → "Require approval for first-time
  contributors".
- [ ] Optional: allow only selected actions. Repository settings → Actions → General →
  Actions permissions → "Allow select actions and reusable workflows", then list the
  actions our workflows use.

## Community profile

- [ ] Check that every item in Community Standards is complete. Insights → Community
  Standards.
- [ ] Before the first release, replace the placeholder contact method in
  `CODE_OF_CONDUCT.md`.
