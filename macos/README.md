# Isolated image decoder

`slack-tooy-image-decoder` is a small, signed Swift/ImageIO process. The Rust TUI
compiles [`decoder.sb`](LocalDecoder/decoder.sb) into its executable and supplies
it automatically when starting an image job. The native launch mode replaces
itself with `/usr/bin/sandbox-exec -p` and that embedded profile, so installation
requires no separate `.sb` file. The decoder receives one bounded image
on stdin and returns straight-alpha sRGB pixels on stdout. It accepts JPEG and
PNG, rejects source rasters over 16,384 pixels per edge or 64 million pixels,
and emits thumbnails with a maximum edge of 2048 pixels.

The profile denies operations by default. It allows the helper's executable,
system framework reads, Apple's Rosetta runtime, metadata queries, read-only sysctls, and existing pipe
I/O. Before applying the profile, the launcher uses Darwin's
`POSIX_SPAWN_CLOEXEC_DEFAULT` to close inherited descriptors other than stdin,
stdout, and stderr. It grants no network access or access to user-file contents. The Rust parent
limits encoded input and decoded output to 16 MiB, validates the response before
allocating, and enforces a 15-second deadline and two concurrent image jobs.

Use `just run` to build, test, sign, and launch the app, or `just build` to stop
after building. Both executables are
placed in `target/release`; `just install` installs them together. Keep them beside
one another when installing elsewhere. On macOS, missing or rejected helpers
leave image placeholders rather than decoding untrusted files inside the TUI.

Build using Xcode 26 or later and XcodeGen:

```sh
xcodegen generate --spec macos/project.yml
xcodebuild -project macos/SlackTooyMedia.xcodeproj -scheme slack-tooy-image-decoder \
  -configuration Release -derivedDataPath target/macos-media CODE_SIGN_IDENTITY=- build
```

The product is `target/macos-media/Build/Products/Release/slack-tooy-image-decoder`.
The configuration enables Hardened Runtime and Enhanced Security. It uses local
ad-hoc signing; Developer ID distribution is a separate signing/notarization task.

The ImageIO conversion itself can be tested without starting the extension:

```sh
xcodebuild -project macos/SlackTooyMedia.xcodeproj -scheme ImageDecoderTests \
  -configuration Release -derivedDataPath target/macos-media-tests CODE_SIGN_IDENTITY=- build
xcrun xctest target/macos-media-tests/Build/Products/Release/ImageDecoderTests.xctest
```

These tests cover supported formats, straight alpha, invalid data, source-raster
and byte limits, and thumbnail dimensions. `scripts/verify-macos-decoder.py`
tests the built helper under the actual profile. Native checks also cover
unrelated file reads, localhost connections, and deliberately inherited file
descriptors, with successful unsandboxed controls.

Apple's supported [App Sandbox configuration](https://developer.apple.com/documentation/security/protecting-user-data-with-app-sandbox)
uses code-signing entitlements. It does not automatically apply this custom
profile merely because the profile is embedded; the native launcher applies it.
Seatbelt's profile language is an Apple system interface and can change between
macOS versions. The limits do not prove that ImageIO's internal allocations have
a strict memory ceiling, or protect against a kernel/sandbox escape. The Apple
Enhanced Security extension approach was also prototyped, but this host rejected
its ad-hoc identity during ExtensionFoundation discovery. That experiment remains
in local `docs/.ai/probes/enhanced-image-decoder/Source/`; production does not
silently fall back from one isolation mechanism to another.
