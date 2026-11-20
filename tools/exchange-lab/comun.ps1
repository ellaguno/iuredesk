# Lo compartido por los pasos: contraseñas de laboratorio (Z:\lab.env) y la unidad de Exchange.
$ErrorActionPreference = 'Stop'
$cfg = ((Get-Content Z:\lab.env) | Where-Object { $_ -match '^[A-Z_]+=' }) -join "`n" | ConvertFrom-StringData
function Secure([string]$s) { ConvertTo-SecureString $s -AsPlainText -Force }
function ExchangeDrive {
    foreach ($d in Get-PSDrive -PSProvider FileSystem) {
        if ((Test-Path "$($d.Root)Setup.exe") -and (Test-Path "$($d.Root)UCMARedist")) { return $d.Root }
    }
    throw 'No encuentro la ISO de Exchange montada (con Setup.exe y UCMARedist).'
}
