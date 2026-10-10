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
```
