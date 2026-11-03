#!/bin/sh
# Builds release executables and packs each into dist/playterm-<version>-macos-<arch>.zip,
# holding playterm.app (unzip it and move playterm.app to Applications).
#
#   scripts/package-macos.sh                  # arm64 (Apple silicon) + x64 (Intel)
#   scripts/package-macos.sh arm64            # one architecture
#   scripts/package-macos.sh universal        # one app for both (lipo)
#   RUSTUP_TOOLCHAIN=1.95 scripts/package-macos.sh   # a toolchain other than the default
#
# The app is signed ad hoc unless $MACOS_SIGN_IDENTITY names a "Developer ID Application"
# certificate in the keychain; it is then signed with the hardened runtime and a timestamp,
# ready for notarization (xcrun notarytool submit <zip> --wait, then xcrun stapler staple).
# An app signed ad hoc and downloaded from the internet is stopped by Gatekeeper on first run
# (right-click → Open, or System Settings → Privacy & Security → Open Anyway).
#
# SHA-256 sums of the zips are written to dist/SHA256SUMS-macos.txt.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

[ "$(uname -s)" = Darwin ] || { echo "package-macos.sh runs on macOS only" >&2; exit 1; }

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

targets=${*:-arm64 x64}
# Oldest macOS the app runs on (also the Rust minimum for Apple silicon).
min_macos=11.0
export MACOSX_DEPLOYMENT_TARGET=$min_macos

triple() {
    case $1 in
        arm64) echo aarch64-apple-darwin ;;
        x64) echo x86_64-apple-darwin ;;
        *) echo "unknown architecture: $1 (arm64, x64 or universal)" >&2; exit 1 ;;
    esac
}

# Builds playterm for one architecture; prints the path of the executable.
build() {
    t=$(triple "$1")
    rustup target add "$t" >/dev/null 2>&1 || true
    echo "== cargo build ($t)" >&2
    cargo build --release --locked -p playterm --target "$t" >&2
    echo "target/$t/release/playterm"
}

# make_app <executable> <app path>
make_app() {
    exe=$1
    app=$2
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$exe" "$app/Contents/MacOS/playterm"
    cp assets/playterm.icns "$app/Contents/Resources/playterm.icns"
    printf 'APPL????' > "$app/Contents/PkgInfo"
    cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>ko</string>
    <key>CFBundleLocalizations</key>
    <array>
        <string>ko</string>
        <string>en</string>
    </array>
    <key>CFBundleDisplayName</key>
    <string>playterm</string>
    <key>CFBundleName</key>
    <string>playterm</string>
    <key>CFBundleExecutable</key>
    <string>playterm</string>
    <key>CFBundleIconFile</key>
    <string>playterm</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.playok.playterm</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key>
    <string>$min_macos</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>MIT License</string>
</dict>
</plist>
EOF
    plutil -lint "$app/Contents/Info.plist" >/dev/null

    if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
        codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$app"
    else
        codesign --force --sign - "$app"
    fi
    codesign --verify --strict "$app"
}

dist="$root/dist"
stage="$root/target/macos-package"
mkdir -p "$dist"
zips=""
for arch in $targets; do
    if [ "$arch" = universal ]; then
        arm=$(build arm64)
        intel=$(build x64)
        exe="$stage/universal/playterm"
        mkdir -p "$stage/universal"
        lipo -create -output "$exe" "$arm" "$intel"
    else
        exe=$(build "$arch")
    fi
    app="$stage/$arch/playterm.app"
    make_app "$exe" "$app"
    zip="playterm-$version-macos-$arch.zip"
    rm -f "$dist/$zip"
    # ditto keeps the bundle's symbolic links and extended attributes (the signature).
    ditto -c -k --sequesterRsrc --keepParent "$app" "$dist/$zip"
    echo "== $zip ($(lipo -archs "$app/Contents/MacOS/playterm"))"
    zips="$zips $zip"
done

(cd "$dist" && shasum -a 256 $zips > SHA256SUMS-macos.txt)
cat "$dist/SHA256SUMS-macos.txt"
