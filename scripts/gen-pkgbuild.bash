#!/bin/bash
# Script to generate PKGBUILD files for Ałycz Linux

set -e  # fail if generating sha fails or smth

txt="# Maintainer: TheLastGimbus <mateusz.soszynski@tuta.io>
pkgname=gpth-bin
pkgver=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
pkgrel=1
pkgdesc='Tool to help you with exporting stuff from Google Photos'
arch=('x86_64')
url='https://github.com/TheLastGimbus/GooglePhotosTakeoutHelper'
license=('Apache')
depends=()
provides=('gpth')
conflicts=('gpth')
options=('!strip')
source=(\"\${url}/releases/download/v\${pkgver}/gpth-linux\")
sha256sums=('$(sha256sum "$1" | cut -d " " -f 1)')

package() {
    install -Dm755 \"gpth-linux\" \"\${pkgdir}/usr/bin/gpth\"
}"

echo "$txt" | tee PKGBUILD
