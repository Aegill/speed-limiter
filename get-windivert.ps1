# Downloads official WinDivert 2.2.2 and copies WinDivert.dll + WinDivert64.sys
# next to the built program (.\target\release by default).
#
# Usage: powershell -ExecutionPolicy Bypass -File .\get-windivert.ps1 [-Target .\target\release]
# (ASCII only on purpose: Windows PowerShell 5.1 misreads UTF-8 files without BOM)

param([string]$Target = ".\target\release")

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$url = "https://github.com/basil00/WinDivert/releases/download/v2.2.2/WinDivert-2.2.2-A.zip"
$tmp = Join-Path $env:TEMP "windivert-2.2.2"

if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
New-Item -ItemType Directory -Path $tmp | Out-Null

Write-Host "Downloading $url ..."
Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp "w.zip") -UseBasicParsing
Expand-Archive -Path (Join-Path $tmp "w.zip") -DestinationPath $tmp -Force

$dll = Get-ChildItem $tmp -Recurse -Filter "WinDivert.dll"   | Where-Object { $_.FullName -match "x64" } | Select-Object -First 1
$sys = Get-ChildItem $tmp -Recurse -Filter "WinDivert64.sys" | Where-Object { $_.FullName -match "x64" } | Select-Object -First 1
if (-not $dll -or -not $sys) { throw "x64\WinDivert.dll or WinDivert64.sys not found in the archive" }

New-Item -ItemType Directory -Path $Target -Force | Out-Null
Copy-Item $dll.FullName $Target -Force
Copy-Item $sys.FullName $Target -Force
$dest = (Resolve-Path $Target).Path
Write-Host "Done: files copied to $dest"
