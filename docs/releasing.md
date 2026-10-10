# Releasing LoopCut

LoopCut ships as a signed Windows NSIS installer on GitHub Releases. The installed app checks
`https://github.com/LunoviaVR/LoopCut/releases/latest/download/latest.json` for updates and installs them after a
signature check. This document covers the one-time setup, the release steps, how updates reach users, rollback,
and the FFmpeg pin bump.

## One-time setup

### 1. Generate the updater signing key

Updates are signed with a minisign-compatible key. Generate it once, on the release machine, in PowerShell. When it asks for a password, leave it empty (the workflow passes an empty password until one is set):

```powershell
npm run tauri signer generate -- -w "$env:USERPROFILE\.tauri\loopcut-updater.key"
```

This writes two files:

- `C:\Users\<user>\.tauri\loopcut-updater.key`: the **private** key. It must never be committed (`*.key` is in
  `.gitignore`), pasted into an issue, or stored in the repository.
- `C:\Users\<user>\.tauri\loopcut-updater.key.pub`: the public key. Its contents go into
  `src-tauri/tauri.conf.json` as `plugins.updater.pubkey`. The installed apps verify every update against it.

Back up the private key **twice, offline** (for example two encrypted USB drives kept in different places). Losing
it means no future update can be signed for the existing installs.

### 2. Add the GitHub repository secrets

In GitHub: *Settings → Secrets and variables → Actions → New repository secret*.

| Secret | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | The full contents of `loopcut-updater.key` (the private key file). |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The key's password. The key is currently generated without one, so leave it empty (or do not create it). |

The release workflow (`.github/workflows/release.yml`) passes both to `tauri-apps/tauri-action`.

### Key rotation

Until the first public release (`v0.1.0`), rotating the key is free: generate a new key, update `pubkey` and the
secrets, and nobody is affected. After a public release, every installed copy carries the old public key, so a
rotated key means users must reinstall manually. Treat the key as permanent once users exist.

## Cutting a release

1. **Make sure `main` is green** in CI, and that `CHANGELOG.md` describes the release. The release body links to it at the release commit, so create the file with the first release.
2. **Bump the version** from the repository root:

   ```powershell
   node scripts/bump-version.mjs 0.1.0
   ```

   This updates `package.json` (the app version; `tauri.conf.json` reads it), `package-lock.json`, and the
   `[workspace.package]` version plus the internal path dependencies in the root `Cargo.toml`. It refuses versions
   that are not plain `MAJOR.MINOR.PATCH` or that are not greater than the current version.
3. **Refresh the lockfile**: `cargo update --workspace` (updates only the workspace crates in `Cargo.lock`).
4. **Commit, tag and push**. The tag must be `v` followed by the `package.json` version exactly:

   ```powershell
   git add -A
   git commit -m "Release v0.1.0"
   git tag -a v0.1.0 -m "LoopCut v0.1.0"
   git push origin main v0.1.0
   ```

5. **Wait for the release workflow.** `release.yml` runs on the tag and:
   - fails early if the tag does not match the `package.json` version;
   - fetches and verifies the pinned FFmpeg build;
   - builds the app with `tauri-apps/tauri-action`, signs the NSIS installer and its `.sig`, and creates a
     **draft** GitHub release with `LoopCut_<version>_x64-setup.exe`, the `.sig` and `latest.json`.
6. **Smoke test the draft** before publishing:
   - download the installer from the draft and install it on a clean Windows 11 VM (or a machine without dev tools);
   - launch it, import a clip, export a GIF and an MP4, and confirm the FFmpeg files are in the install folder;
   - install the previous public version first, then run the new installer over it (upgrade path). Check that the
     app offers the update, installs it, and relaunches on the new version;
   - confirm the draft's `latest.json` has the new version and a non-empty `signature`.
7. **Publish the draft** (*Edit → Publish release*). Only then does the `releases/latest/download/latest.json` URL
   point at the new version.

Dry runs: run the workflow manually (*Actions → Release → Run workflow*). It builds and creates a draft release
from the current commit with the same version rules; delete the draft when you no longer need it.

## How updates reach users

- `tauri-action` uploads `latest.json` to the release with `uploadUpdaterJson: true` and
  `updaterJsonPreferNsis: true`. It lists the version, release notes, publish date, and for `windows-x86_64` the
  NSIS installer URL and its signature.
- GitHub's `releases/latest` only points at the most recent **published, non-prerelease** release. Drafts and
  prereleases are invisible to the updater, so a release is live for updates exactly when you publish it.
- The app checks on startup and every 4 hours, shows the release notes, and installs only when the user chooses.
  The installer must verify against the public key compiled into the app.

## Rollback

The updater refuses downgrades, so a bad release is never rolled back by reinstalling an old version through the
updater. To roll back, fix or revert the code and **publish a higher version** (for example `0.1.1` after a broken
`0.1.0`). If the bad release must stop being offered immediately, edit it to a draft or delete it. Do not reuse or
move a published tag.

## FFmpeg pin bump

LoopCut bundles one pinned FFmpeg build (`scripts/ffmpeg.lock.json`). Changing it is a release-level change.

1. **Pick the build.** Choose a BtbN `win64-gpl-shared` autobuild and note its asset name, size, the FFmpeg commit it
   was built from, and its SHA-256 (`Get-FileHash -Algorithm SHA256 <file>`).
2. **Update the lock file**: `version`, `windows-x86_64.asset`, `sha256`, `size`, `urls` (our mirror first, BtbN
   second), and `source.commit`.
3. **Mirror both artifacts.** BtbN prunes dailies after about 14 days, so upload the zip **and** the matching FFmpeg
   source tarball to a GitHub release named `deps-ffmpeg-<version>` in `LunoviaVR/LoopCut`. The source tarball
   covers the GPL source offer for the bundled binaries.
4. **Re-stage and verify locally:**

   ```powershell
   node scripts/fetch-ffmpeg.mjs --force
   node scripts/verify-ffmpeg.mjs
   node scripts/gen-test-media.mjs --force
   cargo test --workspace
   ```

5. **Update the notices**: `THIRD_PARTY_NOTICES.md` lists the FFmpeg configuration and the linked libraries.
6. **Bump the app version** (`node scripts/bump-version.mjs <next>`) and release. The bundled binaries changed, so
   users must get them through a new installer.

`verify-ffmpeg.mjs` fails the build if the staged build lacks a required encoder, decoder, filter or hardware
method, or if the staged archive does not match the lock file's SHA-256.
