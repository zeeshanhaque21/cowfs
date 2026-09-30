#!/bin/sh
# copy scripts and tests into the VM work dir
S=/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/spike3
D=/home/zeeshanhaque/cowfs-spike3
orb -m cowfs-spike3 sh -c "cd $S && cp *.sh bench.py $D/ && cp test_*.py mmap_race.py $D/tests/ && rm -f $D/push.sh $D/sync.sh $D/vm.sh"
