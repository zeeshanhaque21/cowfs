# Clone vs cowfs workspaces on macOS (WIP)

Work in progress.
Raw CSV so far:

```csv
arm,round,slot,seconds,compiling_count,df_free_kb,notes
A,R1,start,0.00,,244071580,load1=2.50
A,R1,1,12.04,,239260328,rc=0 load1=2.35
A,R1,2,12.04,,234431460,rc=0 load1=2.55
A,R1,3,12.04,,229604336,rc=0 load1=2.80
A,R1,4,12.03,,224777484,rc=0 load1=2.77
A,R1,5,12.04,,219981608,rc=0 load1=2.94
A,R1,6,12.05,,215136676,rc=0 load1=2.60
A,R1,7,12.05,,210308748,rc=0 load1=2.25
A,R1,8,12.04,,205482424,rc=0 load1=2.33
A,R1,end,0.00,,205482316,load1=2.21
B,R1,start,0.00,,205482264,load1=2.40
B,R1,1,8.03,,205487784,rc=0 load1=2.68
B,R1,2,8.03,,205470932,rc=0 load1=2.73
B,R1,3,8.03,,205453872,rc=0 load1=2.79
B,R1,4,8.02,,205446948,rc=0 load1=2.64
B,R1,5,8.03,,205428728,rc=0 load1=2.44
B,R1,6,8.03,,205410564,rc=0 load1=2.47
B,R1,7,8.02,,205393736,rc=0 load1=2.82
B,R1,8,8.02,,205376792,rc=0 load1=3.02
B,R1,end,0.00,,205376568,load1=2.87
C,R1,start,0.00,,205376668,load1=2.58 logical=3710135686 stored=1228528323
C,R1,1,2.01,,205376456,rc=0 load1=2.74 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,2,2.01,,205376404,rc=0 load1=2.85 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,3,2.01,,205376344,rc=0 load1=2.81 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,4,2.00,,205344500,rc=0 load1=2.53 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,5,2.01,,205353420,rc=0 load1=2.41 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,6,2.01,,205352896,rc=0 load1=2.66 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,7,2.01,,205334244,rc=0 load1=2.66 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,8,2.00,,205342292,rc=0 load1=2.72 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,end,0.00,,205342328,load1=2.46 logical=3710135686 stored=1228528323
C,R2,start,0.00,,205336964,load1=2.03 logical=3710135686 stored=1228528323
C,R2,1,63.46,1,205307524,rc=0 load1=2.74 edit=cowfs-cli logical=3769399240 stored=1247319405
C,R2,end,0.00,,205307276,load1=2.24 logical=3769399240 stored=1247319405
A,R2,start,0.00,,205312980,load1=2.43
A,R2,1,17.95,74,203779336,rc=0 load1=9.85 edit=cowfs-cli
A,R2,2,17.50,74,202241996,rc=0 load1=22.65 edit=cowfs-treehouse
A,R2,3,17.55,74,200745676,rc=0 load1=21.24 edit=cowfs-daemon
A,R2,4,17.34,74,199233540,rc=0 load1=29.17 edit=cowfs-fuse
A,R2,5,17.43,74,197703036,rc=0 load1=24.55 edit=cowfs-nfs
A,R2,6,17.95,74,196293080,rc=0 load1=24.81 edit=cowfs-ctl
A,R2,7,17.98,74,194801192,rc=0 load1=20.55 edit=nfsserve
A,R2,8,17.39,74,193324860,rc=0 load1=28.37 edit=cowfs-core
A,R2,end,0.00,,193324476,load1=21.14
B,R2,start,0.00,,193324420,load1=18.50
B,R2,1,3.47,1,193219024,rc=0 load1=17.34 edit=cowfs-cli
B,R2,2,2.73,1,193070852,rc=0 load1=14.57 edit=cowfs-treehouse
B,R2,3,4.16,2,192686468,rc=0 load1=13.05 edit=cowfs-daemon
B,R2,4,1.17,1,192655452,rc=0 load1=12.66 edit=cowfs-fuse
B,R2,5,5.87,3,191789636,rc=0 load1=11.76 edit=cowfs-nfs
B,R2,6,5.50,4,191075324,rc=0 load1=14.15 edit=cowfs-ctl
B,R2,7,6.50,4,190142668,rc=0 load1=13.37 edit=nfsserve
B,R2,8,7.77,5,188388580,rc=0 load1=12.86 edit=cowfs-core
B,R2,end,0.00,,188388560,load1=9.86
C,R2,start,0.00,,188387588,load1=8.79 logical=3769399240 stored=1247319405
C,R2,2,115.75,1,188433364,rc=0 load1=4.18 edit=cowfs-treehouse logical=3885012655 stored=1292257617
C,R2,3,170.06,2,188350780,rc=0 load1=4.43 edit=cowfs-daemon logical=4098077441 stored=1365713825
C,R2,4,42.24,1,188343908,rc=0 load1=4.27 edit=cowfs-fuse logical=4121029745 stored=1374697651
C,R2,5,396.22,3,188185900,rc=0 load1=6.06 edit=cowfs-nfs logical=4559498642 stored=1528117241
C,R2,6,407.99,4,187981860,rc=0 load1=5.82 edit=cowfs-ctl logical=4937418008 stored=1675573016
C,R2,7,449.87,4,187671664,rc=0 load1=3.89 edit=nfsserve logical=5390164818 stored=1845258313
C,R2,8,564.63,5,187438648,rc=101 load1=3.86 edit=cowfs-core logical=6067796280 stored=2073995138
C,R2rerun,start,0.00,,187437352,load1=3.40 logical=6067796280 stored=2073995138
C,R2rerun,8,184.72,4,187361108,rc=0 load1=3.32 diagnostic rerun after failure logical=6300965305 stored=2150533480
C,R2rerun,end,0.00,,187361468,load1=3.08 logical=6300965305 stored=2150533480
```
