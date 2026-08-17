# Консоль Nova Arc для ручных проверок.
# PowerShell (а не .cmd) намеренно: консоль Windows читает .cmd в OEM-кодировке,
# и русский текст в батнике превращается в мусор.
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root 'target\release'
$env:PATH = "$bin;$env:PATH"

if (-not (Test-Path (Join-Path $bin 'narc.exe'))) {
    Write-Host "[!] Программа ещё не собрана. Выполните в папке проекта:" -ForegroundColor Yellow
    Write-Host "    cargo build --release" -ForegroundColor Yellow
    Write-Host ""
}

$work = Join-Path $root 'test'
if (Test-Path $work) { Set-Location $work } else { Set-Location $root }

Write-Host "============================================================" -ForegroundColor DarkCyan
Write-Host "  Nova Arc — консоль для ручных проверок" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor DarkCyan
Write-Host ""
Write-Host "  narc create архив.narc папка -l max   создать (fast/normal/max)"
Write-Host "  narc add    архив.narc папка -l max   досохранить изменения"
Write-Host "  narc list   архив.narc                список файлов"
Write-Host "  narc info   архив.narc                статистика архива"
Write-Host "  narc extract архив.narc -o папка      распаковать"
Write-Host "  narc compact архив.narc               убрать мусор из архива"
Write-Host ""
Write-Host "  Ключи: -j N (потоки), --memory 512M (лимит памяти),"
Write-Host "         --eco (щадящий режим), --full (без ограничений)"
Write-Host "  Справка: narc --help"
Write-Host ""
Write-Host "  Уровень max жмёт сильнее, но заметно медленнее." -ForegroundColor DarkGray
Write-Host "  Папка: $(Get-Location)" -ForegroundColor DarkGray
Write-Host "============================================================" -ForegroundColor DarkCyan
Write-Host ""
