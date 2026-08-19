# Консоль Nova Prism для ручных проверок.
# PowerShell (а не .cmd) намеренно: консоль Windows читает .cmd в OEM-кодировке,
# и русский текст в батнике превращается в мусор.
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root 'target\release'
$env:PATH = "$bin;$env:PATH"

if (-not (Test-Path (Join-Path $bin 'nova.exe'))) {
    Write-Host "[!] Программа ещё не собрана. Выполните в папке проекта:" -ForegroundColor Yellow
    Write-Host "    cargo build --release" -ForegroundColor Yellow
    Write-Host ""
}

$work = Join-Path $root 'test'
if (Test-Path $work) { Set-Location $work } else { Set-Location $root }

Write-Host "============================================================" -ForegroundColor DarkCyan
Write-Host "  Nova Prism — консоль для ручных проверок" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor DarkCyan
Write-Host ""
Write-Host "  nova create архив.nva папка -l max   создать (fast/normal/max)"
Write-Host "  nova add    архив.nva папка -l max   досохранить изменения"
Write-Host "  nova list   архив.nva                список файлов"
Write-Host "  nova info   архив.nva                статистика архива"
Write-Host "  nova extract архив.nva -o папка      распаковать"
Write-Host "  nova compact архив.nva               убрать мусор из архива"
Write-Host ""
Write-Host "  Ключи: -j N (потоки), --memory 512M (лимит памяти),"
Write-Host "         --eco (щадящий режим), --full (без ограничений)"
Write-Host "  Справка: nova --help"
Write-Host ""
Write-Host "  Уровень max жмёт сильнее, но заметно медленнее." -ForegroundColor DarkGray
Write-Host "  Папка: $(Get-Location)" -ForegroundColor DarkGray
Write-Host "============================================================" -ForegroundColor DarkCyan
Write-Host ""
