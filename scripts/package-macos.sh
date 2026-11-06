#!/bin/sh
# Builds release executables and packs each into dist/playterm-<version>-macos-<arch>.dmg (a disk
# image holding playterm.app and an Applications link to drag it onto) and
# dist/playterm-<version>-macos-<arch>.zip (the same app; the in-app update check looks for it).
#
#   scripts/package-macos.sh                  # arm64 (Apple silicon) + x64 (Intel)
#   scripts/package-macos.sh arm64            # one architecture
#   scripts/package-macos.sh universal        # one app for both (lipo)
#   RUSTUP_TOOLCHAIN=1.95 scripts/package-macos.sh   # a toolchain other than the default
#   scripts/package-macos.sh --build-only arm64 x64    # only build
#   scripts/package-macos.sh --skip-build arm64 x64    # package what --build-only built (no cargo)
#
# Certificates and notarization passwords should not be in the environment (or the keychain)
# while cargo builds: every dependency's build script runs then. The release workflow builds with
# --build-only first, then imports the certificate and packages with --skip-build.
#
# The app is signed ad hoc unless $MACOS_SIGN_IDENTITY names a "Developer ID Application"
# certificate in the keychain; it is then signed with the hardened runtime and a timestamp.
# With $MACOS_NOTARY_APPLE_ID, $MACOS_NOTARY_TEAM_ID and $MACOS_NOTARY_PASSWORD (an app-specific
# password) too, each app is notarized (xcrun notarytool submit --wait), the ticket stapled to it
# and Gatekeeper's verdict checked, before it is zipped again. The app needs no entitlements under
# the hardened runtime (no JIT, no unsigned libraries; network and serial ports need none outside
# the App Sandbox).
# An app signed ad hoc and downloaded from the internet is stopped by Gatekeeper on first run
# (right-click → Open, or System Settings → Privacy & Security → Open Anyway).
#
# With a Developer ID the disk image is signed too, and with the notarization secrets notarized
# and stapled as well. Without them it is not signed (an ad hoc signature means nothing to
# Gatekeeper on a disk image); the app inside is still signed as above.
#
# SHA-256 sums of the disk images and zips are written to dist/SHA256SUMS-macos.txt.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

[ "$(uname -s)" = Darwin ] || { echo "package-macos.sh runs on macOS only" >&2; exit 1; }

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

step=all
case "${1:-}" in
    --build-only) step=build; shift ;;
    --skip-build) step=package; shift ;;
esac
targets=${*:-arm64 x64}

notarize=
if [ -n "${MACOS_NOTARY_APPLE_ID:-}${MACOS_NOTARY_TEAM_ID:-}${MACOS_NOTARY_PASSWORD:-}" ]; then
    [ -n "${MACOS_NOTARY_APPLE_ID:-}" ] && [ -n "${MACOS_NOTARY_TEAM_ID:-}" ] && [ -n "${MACOS_NOTARY_PASSWORD:-}" ] ||
        { echo "notarization needs MACOS_NOTARY_APPLE_ID, MACOS_NOTARY_TEAM_ID and MACOS_NOTARY_PASSWORD" >&2; exit 1; }
    [ -n "${MACOS_SIGN_IDENTITY:-}" ] ||
        { echo "notarization needs a Developer ID signature (MACOS_SIGN_IDENTITY)" >&2; exit 1; }
    notarize=1
fi

# notarize_app <app> <zip>: has Apple notarize the zipped app, staples the ticket to the app and
# checks that Gatekeeper accepts it.
notarize_app() {
    echo "== notarizing $(basename "$2")"
    result=$(xcrun notarytool submit "$2" --apple-id "$MACOS_NOTARY_APPLE_ID" --team-id "$MACOS_NOTARY_TEAM_ID" \
        --password "$MACOS_NOTARY_PASSWORD" --wait --timeout 1h --output-format plist)
    status=$(printf '%s' "$result" | plutil -extract status raw -o - -)
    if [ "$status" != Accepted ]; then
        id=$(printf '%s' "$result" | plutil -extract id raw -o - - 2>/dev/null || true)
        echo "notarization: $status" >&2
        [ -n "$id" ] && xcrun notarytool log "$id" --apple-id "$MACOS_NOTARY_APPLE_ID" \
            --team-id "$MACOS_NOTARY_TEAM_ID" --password "$MACOS_NOTARY_PASSWORD" >&2
        exit 1
    fi
    xcrun stapler staple "$1"
    xcrun stapler validate "$1"
    spctl --assess --type execute --verbose=2 "$1"
}
# make_dmg <app> <dmg>: packs the (final, stapled) app into a compressed disk image with an
# Applications link, signs, notarizes and staples it when the secrets allow, and checks the app
# inside the image.
make_dmg() {
    src="$stage/$arch/dmg-root"
    rm -rf "$src" "$2"
    mkdir -p "$src"
    ditto "$1" "$src/playterm.app"
    ln -s /Applications "$src/Applications"
    hdiutil create -quiet -volname playterm -srcfolder "$src" -fs HFS+ -format UDZO -ov "$2"
    rm -rf "$src"
    if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
        codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$2"
        codesign --verify --strict --verbose=2 "$2"
    fi
    if [ -n "$notarize" ]; then
        echo "== notarizing $(basename "$2")"
        result=$(xcrun notarytool submit "$2" --apple-id "$MACOS_NOTARY_APPLE_ID" --team-id "$MACOS_NOTARY_TEAM_ID" \
            --password "$MACOS_NOTARY_PASSWORD" --wait --timeout 1h --output-format plist)
        status=$(printf '%s' "$result" | plutil -extract status raw -o - -)
        if [ "$status" != Accepted ]; then
            echo "notarization: $status" >&2
            exit 1
        fi
        xcrun stapler staple "$2"
        xcrun stapler validate "$2"
        spctl --assess --type open --context context:primary-signature --verbose=2 "$2"
    fi
    hdiutil verify -quiet "$2"
    mnt=$(mktemp -d "$stage/$arch/dmg-mount.XXXXXX")
    hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mnt" "$2"
    ok=1
    [ -L "$mnt/Applications" ] || { echo "$(basename "$2"): no Applications link" >&2; ok=; }
    codesign --verify --deep --strict --verbose=2 "$mnt/playterm.app" || ok=
    if [ -n "$notarize" ]; then
        xcrun stapler validate "$mnt/playterm.app" || ok=
    fi
    hdiutil detach -quiet "$mnt"
    rmdir "$mnt"
    [ -n "$ok" ] || exit 1
}

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

# Builds playterm for one architecture (with --skip-build: checks it was built); prints the
# path of the executable.
build() {
    t=$(triple "$1")
    exe="target/$t/release/playterm"
    if [ "$step" = package ]; then
        [ -f "$exe" ] || { echo "--skip-build: $exe is not built (run --build-only first)" >&2; exit 1; }
    else
        rustup target add "$t" >/dev/null 2>&1 || true
        echo "== cargo build ($t)" >&2
        cargo build --release --locked -p playterm --target "$t" >&2
    fi
    echo "$exe"
}

if [ "$step" = build ]; then
    for arch in $targets; do
        if [ "$arch" = universal ]; then build arm64 >/dev/null; build x64 >/dev/null; else build "$arch" >/dev/null; fi
    done
    echo "built; package with --skip-build"
    exit 0
fi

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
    if [ -n "$notarize" ]; then
        notarize_app "$app" "$dist/$zip"
        # Again, with the ticket in the app (it then opens without asking Apple first).
        rm -f "$dist/$zip"
        ditto -c -k --sequesterRsrc --keepParent "$app" "$dist/$zip"
    fi
    # Verify what will actually be uploaded, after the final ZIP and stapling step.
    verify=$(mktemp -d "$stage/$arch/final-verify.XXXXXX")
    ditto -x -k "$dist/$zip" "$verify"
    codesign --verify --deep --strict --verbose=2 "$verify/playterm.app"
    codesign --display --verbose=4 "$verify/playterm.app" 2>&1
    if [ -n "$notarize" ]; then
        xcrun stapler validate "$verify/playterm.app"
        spctl --assess --type execute --verbose=2 "$verify/playterm.app"
    fi
    dmg="playterm-$version-macos-$arch.dmg"
    make_dmg "$app" "$dist/$dmg"
    echo "== $dmg, $zip ($(lipo -archs "$app/Contents/MacOS/playterm"))"
    zips="$zips $dmg $zip"
done

(cd "$dist" && shasum -a 256 $zips > SHA256SUMS-macos.txt)
cat "$dist/SHA256SUMS-macos.txt"
