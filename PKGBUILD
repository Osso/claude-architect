# Maintainer: Alessio Deiana <adeiana@gmail.com>
pkgname=claude-architect
pkgver=0.1.0
pkgrel=1
pkgdesc="Task decomposition validator for Claude Code"
arch=('x86_64')
license=('MIT')
makedepends=('cargo')
source=()

build() {
    cd "$startdir"
    cargo build --release --locked
}

package() {
    cd "$startdir"
    install -Dm755 "target/release/claude-architect" "$pkgdir/usr/bin/claude-architect"
    install -Dm755 "target/release/claude-architect-hook" "$pkgdir/usr/bin/claude-architect-hook"
    install -Dm755 "target/release/claude-architect-mcp" "$pkgdir/usr/bin/claude-architect-mcp"
    install -Dm755 "target/release/claude-architect-ctl" "$pkgdir/usr/bin/claude-architect-ctl"
    install -Dm644 "claude-architect.service" "$pkgdir/usr/lib/systemd/user/claude-architect.service"
}
