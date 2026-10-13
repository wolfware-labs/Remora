# Creates a ReFS Dev Drive on a dynamically expanding VHDX (spec 17.5) and writes
# a test directory on it to the step output `path`. The file grows only as data
# is written, so the 50 GB size (the Dev Drive minimum) costs no disk up front.
param(
    [string]$Vhd = (Join-Path $env:RUNNER_TEMP 'devdrive.vhdx'),
    [int]$SizeMB = 51200,
    [string]$Letter = 'R'
)
$ErrorActionPreference = 'Stop'

$script = Join-Path $env:RUNNER_TEMP 'devdrive.txt'
@(
    "create vdisk file=`"$Vhd`" maximum=$SizeMB type=expandable",
    "select vdisk file=`"$Vhd`"",
    'attach vdisk',
    'create partition primary',
    "assign letter=$Letter"
) | Set-Content -Path $script -Encoding ascii
diskpart /s $script
if ($LASTEXITCODE -ne 0) { throw "diskpart failed with exit code $LASTEXITCODE" }

Format-Volume -DriveLetter $Letter -DevDrive -Confirm:$false -Force | Format-List
fsutil devdrv query "${Letter}:"
if ($LASTEXITCODE -ne 0) { throw "fsutil devdrv query failed with exit code $LASTEXITCODE" }

$root = "${Letter}:\remora-test"
New-Item -ItemType Directory -Path $root | Out-Null
"path=$root" | Add-Content -Path $env:GITHUB_OUTPUT
