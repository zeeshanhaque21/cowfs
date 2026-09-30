"""usage: launch.py <log> <cmd...>  start a detached process, write its Mac pid to out/spike3/macpid"""
import subprocess, sys
O = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3"
p = subprocess.Popen(sys.argv[2:], stdout=open(sys.argv[1], "ab"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
open(f"{O}/{sys.argv[1].split('/')[-1]}.pid", "w").write(str(p.pid))
open(f"{O}/macpid", "w").write(str(p.pid))
print(p.pid)
