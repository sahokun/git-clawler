param(
    [string]$TargetDir = "target/release",
    [string]$OutputZip = "target/release/git-clawler-windows-x86_64-portable.zip"
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$targetPath = Join-Path $repoRoot $TargetDir
$zipPath = Join-Path $repoRoot $OutputZip
$exe = Join-Path $targetPath "git-clawler-gui.exe"
$resources = Join-Path $targetPath "resources"
$stagingRoot = Join-Path $targetPath "portable"
$appRoot = Join-Path $stagingRoot "git-clawler"

if (!(Test-Path $exe -PathType Leaf)) {
    throw "Missing GUI executable: $exe"
}

if (!(Test-Path $resources -PathType Container)) {
    throw "Missing GUI resources directory: $resources"
}

Remove-Item -Recurse -Force $stagingRoot -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $appRoot | Out-Null

Copy-Item (Join-Path $repoRoot "LICENSE") $appRoot
Copy-Item (Join-Path $repoRoot "THIRD_PARTY_NOTICES.txt") $appRoot

Copy-Item $exe (Join-Path $appRoot "git-clawler-gui.exe")
Copy-Item $resources (Join-Path $appRoot "resources") -Recurse

Remove-Item -Force $zipPath -ErrorAction SilentlyContinue
Compress-Archive -Path $appRoot -DestinationPath $zipPath -CompressionLevel Optimal -Force
Remove-Item -Recurse -Force $stagingRoot -ErrorAction SilentlyContinue

Write-Output "Created portable zip: $zipPath"