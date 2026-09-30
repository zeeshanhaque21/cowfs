import os, time
out = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3/macload.log"
while True:
    with open(out, "a") as f:
        f.write(f"{time.time():.1f} {os.getloadavg()[0]:.2f}\n")
    time.sleep(5)
