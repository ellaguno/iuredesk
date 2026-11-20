# Paso 4 (VM, IUREDESK\Administrator): instala Exchange (esquema, AD, buzón). 30–60 min.
. Z:\comun.ps1
$ex = ExchangeDrive
$setup = "$($ex)Setup.exe"
$terms = '/IAcceptExchangeServerLicenseTerms_DiagnosticDataOFF'
foreach ($a in @(@('/PrepareSchema'), @('/PrepareAD', '/OrganizationName:IureDesk'), @('/Mode:Install', '/Roles:Mailbox'))) {
    Write-Host "Setup.exe $($a -join ' ')"
    & $setup $terms @a
    if ($LASTEXITCODE -ne 0) { throw "Setup.exe $($a -join ' ') falló ($LASTEXITCODE); mira C:\ExchangeSetupLogs" }
}
Write-Host 'Exchange instalado; reiniciando.'
Restart-Computer -Force
