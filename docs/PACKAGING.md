# Packaging Tandem for macOS

## Build

Prerequisites: Rust toolchain, Xcode command line tools, `cargo install cargo-bundle`.

```sh
export CARGO_TARGET_DIR=$PWD/target/app   # shared cache
scripts/build-dmg.sh
```

This runs `cargo bundle --release -p tandem-app`, ad-hoc signs the bundle, and writes:

- `target/app/release/bundle/osx/Tandem.app`
- `target/app/release/bundle/Tandem-<version>-<arch>.dmg` (app + `/Applications` symlink, UDZO)

Bundle metadata (name, identifier `dev.tandem.ide`, icon, `tandem://` URL scheme, minimum macOS 14.0)
lives in `[package.metadata.bundle]` in `crates/tandem-app/Cargo.toml`. Fonts are embedded with
`include_bytes!`, so no resource copying is needed.

Regenerate the icon (needs Pillow and `iconutil`): `python3 scripts/gen-icon.py` -> `assets/icon/tandem.icns`.

For development use `scripts/dev.sh [project-path]` (debug build).

Note: when launched from Finder the process cwd is `/`, so Tandem opens `/` as the project unless a
path is passed (`open -a Tandem --args /path/to/project`) or a project picker is added.

## Gatekeeper and ad-hoc builds

The default build is signed ad-hoc (`codesign --sign -`), which is not a trusted identity. On another
Mac, Gatekeeper will refuse to open a downloaded copy with a double-click. Right-click (or Ctrl-click)
`Tandem.app` -> Open -> Open, once. Alternatively: `xattr -dr com.apple.quarantine /Applications/Tandem.app`.

## Signing with a Developer ID

Requires a paid Apple Developer account and a "Developer ID Application" certificate in the keychain.

```sh
security find-identity -v -p codesigning        # find the exact identity name
CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" scripts/build-dmg.sh
```

With an identity set, the script signs with `--options runtime --timestamp` (hardened runtime, required
for notarization). If Tandem later needs entitlements (e.g. JIT, unsigned executable memory), add
`--entitlements entitlements.plist` to the `codesign` call. Sign the DMG too:

```sh
codesign --force --sign "$CODESIGN_IDENTITY" --timestamp target/app/release/bundle/Tandem-0.1.0-arm64.dmg
```

## Notarization

One-time: store credentials (use an app-specific password from appleid.apple.com).

```sh
xcrun notarytool store-credentials tandem-notary \
  --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
```

Submit, wait, and staple:

```sh
DMG=target/app/release/bundle/Tandem-0.1.0-arm64.dmg
xcrun notarytool submit "$DMG" --keychain-profile tandem-notary --wait
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature -v "$DMG"
```

If submission is rejected: `xcrun notarytool log <submission-id> --keychain-profile tandem-notary`.
