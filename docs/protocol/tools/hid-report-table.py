d=open('docs/protocol/captures/ups-hid-report-descriptor.bin','rb').read()
i=0;page=None;rid=None;usages=[];out=[]
rsize=rcount=lmin=lmax=None
def flags(v):
    f=["Const" if v&1 else "Data","Var" if v&2 else "Array"]
    if v&0x80: f.append("VOLATILE")
    return ",".join(f)
while i < len(d):
    b=d[i];sz=b&3;sz=4 if sz==3 else sz
    val=int.from_bytes(d[i+1:i+1+sz],'little') if sz else 0
    tag=b&0xfc
    if   tag==0x04: page=val
    elif tag==0x08: usages.append(val)
    elif tag==0x84: rid=val
    elif tag==0x74: rsize=val
    elif tag==0x94: rcount=val
    elif tag==0x14: lmin=val
    elif tag==0x24: lmax=val
    elif tag in (0x80,0x90,0xb0):
        kind={0x80:"Input",0x90:"Output",0xb0:"Feature"}[tag]
        out.append((rid,page,usages[0] if usages else None,kind,rsize,rcount,lmin,lmax,val))
        usages=[]
    i += 1 + sz          # <-- the line I dropped
print(f"{'Rpt':>4} {'Page':>5} {'Usage':>6} {'Kind':<8} {'Sz':>3} {'LMin':>6} {'LMax':>6}  Flags")
seen=set()
for rid,page,u,kind,sz_,cnt,lo,hi,fl in out:
    if u is None or (rid,u,kind) in seen: continue
    seen.add((rid,u,kind))
    print(f"{rid:>4} {page:#5x} {u:#6x} {kind:<8} {sz_ or 0:>3} {lo if lo is not None else '-':>6} {hi if hi is not None else '-':>6}  {flags(fl)}")
