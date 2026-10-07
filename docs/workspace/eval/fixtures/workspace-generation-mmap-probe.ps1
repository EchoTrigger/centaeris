param([Parameter(Mandatory = $true)][string]$ExecutionImage)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$generationProbeName='centaeris-generation-mmap-qc-' + [Guid]::NewGuid().ToString('N').Substring(0, 10)
$generationProbeContainerId = docker run -d --rm --name $generationProbeName --cap-drop ALL --cap-add CHOWN --security-opt no-new-privileges --network none --read-only --tmpfs /mnt/data:rw,nosuid,nodev,size=64m,mode=1777 --tmpfs /run/centaeris:rw,nosuid,nodev,size=8m,mode=0700 --tmpfs /tmp:rw,nosuid,nodev,size=16m,mode=1777 --entrypoint /opt/centaeris/bin/execution_agent $ExecutionImage workspace-watch
if ($LASTEXITCODE -ne 0 -or $generationProbeContainerId -notmatch '^[a-f0-9]{64}$') { throw 'probe container was not created' }
try {
docker exec --user 10001:10001 $generationProbeName python -c "open('/mnt/data/mmap-probe','wb').write(b'A'*4096)"
$generationProbeInitial=docker exec $generationProbeName /opt/centaeris/bin/execution_agent workspace-generation
$generationProbeCode=@'
import ctypes,os,signal
libc=ctypes.CDLL(None,use_errno=True)
libc.mmap.restype=ctypes.c_void_p
libc.mmap.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_int,ctypes.c_int,ctypes.c_int,ctypes.c_long]
libc.msync.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_int]
fd=os.open('/mnt/data/mmap-probe',os.O_RDWR)
region=libc.mmap(None,4096,3,1,fd,0)
assert region != ctypes.c_void_p(-1).value
os.close(fd)
def write(_signal,_frame):
    ctypes.memset(region,ord('B'),1)
    assert libc.msync(region,4096,4)==0
    print('changed:B',flush=True)
signal.signal(signal.SIGUSR1,write)
print('ready:'+str(os.getpid()),flush=True)
while True: signal.pause()
'@
$generationProbeStart=[System.Diagnostics.ProcessStartInfo]::new()
$generationProbeStart.FileName='docker'
$generationProbeStart.UseShellExecute=$false
$generationProbeStart.CreateNoWindow=$true
$generationProbeStart.RedirectStandardOutput=$true
$generationProbeStart.RedirectStandardError=$true
foreach($generationProbeArg in @('exec','--user','10001:10001',$generationProbeName,'/usr/bin/timeout','--signal=TERM','--kill-after=1s','10s','bash','-c','python -u -c "$1" </dev/null >/tmp/mmap-writer.log 2>&1 &','probe',$generationProbeCode)){[void]$generationProbeStart.ArgumentList.Add($generationProbeArg)}
    $generationProbeProcess=[System.Diagnostics.Process]::Start($generationProbeStart)
    if(-not $generationProbeProcess.WaitForExit(5000)){throw 'foreground command did not return'}
    $generationProbeReadiness=@'
import pathlib,time
for _ in range(500):
    p=pathlib.Path('/tmp/mmap-writer.log')
    text=p.read_text() if p.exists() else ''
    if text.startswith('ready:'):
        print(text.splitlines()[0].split(':')[1]);break
    time.sleep(.01)
else: raise RuntimeError('writer readiness timeout')
'@
    $generationWriterPid=docker exec $generationProbeName python -c $generationProbeReadiness
    $generationProbeBefore=docker exec $generationProbeName /opt/centaeris/bin/execution_agent workspace-generation
    docker exec --user 10001:10001 $generationProbeName python -c "import os,signal;os.kill(int('$generationWriterPid'),signal.SIGUSR1)"
    $generationProbeAck=docker exec $generationProbeName /bin/cat /tmp/mmap-writer.log
    $generationProbeAfter=docker exec $generationProbeName /opt/centaeris/bin/execution_agent workspace-generation
    docker exec --user 10001:10001 $generationProbeName /opt/centaeris/bin/execution_agent quiesce-agent-processes
    $generationProbeQuiesced=docker exec $generationProbeName /opt/centaeris/bin/execution_agent workspace-generation
    $generationProbeContent=docker exec $generationProbeName python -c "print(open('/mnt/data/mmap-probe','rb').read(1).decode())"
    docker exec --user 10001:10001 $generationProbeName /usr/bin/timeout --signal=TERM --kill-after=1s 10s bash -c ':'
    $generationProbeForeground=docker exec $generationProbeName /opt/centaeris/bin/execution_agent workspace-generation
    $generationInitialObject=$generationProbeInitial|ConvertFrom-Json
    $generationBeforeObject=$generationProbeBefore|ConvertFrom-Json
    $generationAfterObject=$generationProbeAfter|ConvertFrom-Json
    $generationQuiescedObject=$generationProbeQuiesced|ConvertFrom-Json
    $generationForegroundObject=$generationProbeForeground|ConvertFrom-Json
    if($generationProbeProcess.ExitCode -ne 0){throw 'foreground timeout wrapper failed'}
    if($null -eq $generationInitialObject.generation){throw 'initial workspace must be Known'}
    if($null -ne $generationBeforeObject.generation -or $null -ne $generationAfterObject.generation){throw 'live mmap writer must make generation Unknown'}
    if($null -eq $generationQuiescedObject.generation -or $generationQuiescedObject.generation.instanceEpoch -ne $generationInitialObject.generation.instanceEpoch -or $generationQuiescedObject.generation.generation -le $generationInitialObject.generation.generation){throw 'quiesce must restore Known with close-write increment'}
    if($null -eq $generationForegroundObject.generation -or $generationForegroundObject.generation.generation -ne $generationQuiescedObject.generation.generation){throw 'completed readonly foreground bash must allow Known reuse'}
    if($generationProbeContent -ne 'B'){throw 'mapped file content was not changed'}
    [pscustomobject]@{foregroundExitCode=$generationProbeProcess.ExitCode;initial=$generationInitialObject;beforeWrite=$generationBeforeObject;writeAck=$generationProbeAck;afterWrite=$generationAfterObject;afterQuiesce=$generationQuiescedObject;actualContent=$generationProbeContent;afterReadonlyForeground=$generationForegroundObject;assertionsPassed=$true}|ConvertTo-Json -Depth 5 -Compress
} finally {
    $generationProbeResolvedId = docker ps --all --quiet --no-trunc --filter "id=$generationProbeContainerId"
    if ($generationProbeResolvedId -eq $generationProbeContainerId) {
        docker rm --force $generationProbeContainerId | Out-Null
    }
}
