#!/bin/sh
# The network core lives in ../horizon-voicechat-net (a plain crate, so it can be tested on its own).
# The mod cannot depend on a path crate, so its sources are copied into api/src/net/.
# Run this after changing that crate: sh tools/sync-net.sh ../horizon-voicechat-net
set -e
src="${1:-../horizon-voicechat-net}/src"
dst="$(dirname "$0")/../api/src/net"
mkdir -p "$dst"
cp "$src"/varint.rs "$src"/tcp.rs "$src"/udp.rs "$src"/session.rs "$dst"/
cp "$src"/lib.rs "$dst"/mod.rs
echo "copied $src to $dst"
