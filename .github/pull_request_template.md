## Summary

<!-- What does this change, and why? Link the issue it closes. -->

Closes #

## Type of change

<!-- Match the type in the commit message. -->

- [ ] `feat`: a new feature
- [ ] `fix`: a bug fix
- [ ] `refactor`, `perf` or `style`: no change in behaviour
- [ ] `docs`, `test`, `build`, `ci` or `chore`
- [ ] Breaking change (the commit has `!` or a `BREAKING CHANGE:` footer)

## How it was tested

<!-- Name the engine and server version, and say whether you tried it in the app window, the browser preview, or only with tests. -->

## Screenshots

<!-- For UI changes, before and after. Remove this section otherwise. -->

## Checklist

- [ ] `npm run check` passes
- [ ] Commits follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) (see [docs/CONVENTIONS.md](../docs/CONVENTIONS.md))
- [ ] New logic has a test, and a live test for an engine says what it needs in its `#[ignore]` reason
- [ ] Anything that runs user SQL goes through safe mode in the backend (`readonly::is_read_only` and `safety::check`)
- [ ] Docs are updated when behaviour, setup or the layout changed
- [ ] No passwords, tokens or private data in the diff, the screenshots or the logs
- [ ] A new dependency has a reason in `Cargo.toml` and passes `npm run audit`
