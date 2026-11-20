# Paso 5b (VM, IUREDESK\Administrator): direcciones de correo en iuredesk.internal.
# Exchange no entrega correo a direcciones de dominios reservados por la RFC 2606 (.test, .example…:
# «501 5.1.4 Recipient address reserved by RFC 2606»). Los buzones reciben como dirección principal
# una de iuredesk.internal (.internal está reservado por ICANN para redes privadas, no por la RFC
# 2606) y conservan la de iuredesk.test como secundaria y como nombre de inicio de sesión (UPN):
# como en muchas empresas, el usuario y el correo son distintos.
. Z:\comun.ps1
Add-PSSnapin Microsoft.Exchange.Management.PowerShell.SnapIn
if (-not (Get-AcceptedDomain iuredesk.internal -ErrorAction SilentlyContinue)) {
    New-AcceptedDomain -Name iuredesk.internal -DomainName iuredesk.internal -DomainType Authoritative | Out-Null
}
# Nombres con acento: los scripts anteriores sin marca UTF-8 los dejaron como «PÃ©rez».
foreach ($u in @(@{ A = 'ana'; N = 'Ana'; L = 'Pérez' }, @{ A = 'bea'; N = 'Bea'; L = 'Ruiz' })) {
    Set-User $u.A -FirstName $u.N -LastName $u.L -DisplayName "$($u.N) $($u.L)" -Name "$($u.N) $($u.L)"
}
foreach ($a in @('ana', 'bea')) {
    Set-Mailbox $a -EmailAddressPolicyEnabled $false -PrimarySmtpAddress "$a@iuredesk.internal"
    Set-Mailbox $a -EmailAddresses @{ Add = "smtp:$a@iuredesk.test" }
    Get-Mailbox $a | Format-List PrimarySmtpAddress, UserPrincipalName, EmailAddresses
}
Write-Host 'Listo: correo en @iuredesk.internal, inicio de sesión con @iuredesk.test.'
