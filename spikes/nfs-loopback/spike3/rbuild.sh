. /home/zeeshanhaque/cowfs-spike3/env.sh
cd /home/zeeshanhaque/cowfs-spike3/src/fusepass
CARGO_TARGET_DIR=/home/zeeshanhaque/cowfs-spike3/target-fusepass cargo build --release -j4 2>&1 | grep -v "^\s*Compiling\|^\s*Downloaded" | head -${1:-80}
