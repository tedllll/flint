# Drives the release binary through a real interrupt.
#
# A plain pipe cannot express "start typing in the middle of a turn", so this
# appends to the file the process is reading from: the read blocks until the
# next line exists, which is exactly the behaviour of someone typing later.
#
# ASCII only, and written with a BOM: Windows PowerShell 5.1 reads .ps1 files as
# ANSI unless a BOM says otherwise, which mangles non-ASCII literals.
$ErrorActionPreference = "Continue"

$exe = Join-Path $env:USERPROFILE "bin\flint.exe"
$feed = Join-Path $env:TEMP "flint-interrupt-feed.txt"
$out = Join-Path $env:TEMP "flint-interrupt-out.txt"
$err = Join-Path $env:TEMP "flint-interrupt-err.txt"

Remove-Item $feed, $out, $err -Force -ErrorAction SilentlyContinue

# The two prompts live in separate UTF-8 files so no shell escaping is involved.
$slow = Join-Path $env:TEMP "flint-prompt-slow.txt"
$steer = Join-Path $env:TEMP "flint-prompt-steer.txt"
[System.IO.File]::WriteAllText($slow, "Write a long, detailed 800-word essay about the Rust ownership model. Think carefully between paragraphs.", [System.Text.Encoding]::UTF8)
[System.IO.File]::WriteAllText($steer, "Stop. Do not write the essay. Reply with exactly: INTERRUPT-OK", [System.Text.Encoding]::UTF8)

Copy-Item $slow $feed -Force

$env:DEEPSEEK_API_KEY = [Environment]::GetEnvironmentVariable("DEEPSEEK_API_KEY", "User")

$p = Start-Process -FilePath $exe -ArgumentList @("--no-color") `
    -RedirectStandardInput $feed -RedirectStandardOutput $out -RedirectStandardError $err `
    -NoNewWindow -PassThru

Write-Output "started flint pid=$($p.Id)"
Start-Sleep -Seconds 3

Write-Output "--- appending an interrupt ---"
[System.IO.File]::AppendAllText($feed, "`r`n" + [System.IO.File]::ReadAllText($steer), [System.Text.Encoding]::UTF8)

Start-Sleep -Seconds 20
[System.IO.File]::AppendAllText($feed, "`r`n/exit`r`n", [System.Text.Encoding]::UTF8)

if (-not $p.WaitForExit(60000)) {
    $p.Kill()
    Write-Output "flint did not exit; killed"
} else {
    Write-Output "flint exited with $($p.ExitCode)"
}

Write-Output ""
Write-Output "=== stdout ==="
[System.IO.File]::ReadAllText($out, [System.Text.Encoding]::UTF8)
Write-Output ""
Write-Output "=== stderr ==="
[System.IO.File]::ReadAllText($err, [System.Text.Encoding]::UTF8)
