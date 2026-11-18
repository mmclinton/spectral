# Runs as root in archlinux:base-devel under `bash -euo pipefail`. /in holds
# the PKGBUILD and the source tarball; the built package goes to /out.
pacman -Syu --noconfirm --needed namcap
useradd --create-home builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
# makepkg refuses root, and files the build user wrote straight into a bind
# mount would land on the host under an unmapped subordinate uid, so the
# build happens inside the container and only root copies results out.
install -d -o builder /home/builder/build
install -o builder -m 644 -t /home/builder/build /in/*
cd /home/builder/build
sudo -u builder makepkg --syncdeps --noconfirm
cp ./*.pkg.tar.zst /out/
# Findings are triaged by hand, never a reason to stop.
namcap PKGBUILD || true
namcap ./*.pkg.tar.zst || true
