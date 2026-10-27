; Registro de IureDesk en «Aplicaciones predeterminadas» de Windows 10/11 (correo mailto: y
; archivos .eml, .ics, .vcf). cargo-packager inserta este texto en su installer.nsi como
; `preinstall_section` (ver `cargo xtask package windows`); las secciones «un.» van al desinstalador.
; Windows no deja que un instalador se imponga como predeterminado: aquí solo se declara la
; capacidad; el usuario la elige en Configuración → Aplicaciones → Aplicaciones predeterminadas.
; Los ProgID IureDesk.eml/.ics/.vcf los crea cargo-packager a partir de `file_associations`.

!define IUREDESK_CAPS "Software\Iurefficient\IureDesk\Capabilities"

Section IureDeskDefaultApps
  ; Protocolo mailto: con su propio ProgID (no se pisa HKCR\mailto de otra aplicación).
  WriteRegStr SHCTX "Software\Classes\IureDesk.mailto" "" "URL:MailTo Protocol"
  WriteRegStr SHCTX "Software\Classes\IureDesk.mailto" "URL Protocol" ""
  WriteRegStr SHCTX "Software\Classes\IureDesk.mailto\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
  WriteRegStr SHCTX "Software\Classes\IureDesk.mailto\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""

  WriteRegStr SHCTX "${IUREDESK_CAPS}" "ApplicationName" "IureDesk"
  WriteRegStr SHCTX "${IUREDESK_CAPS}" "ApplicationDescription" "Mail, calendar, contacts, notes and tasks"
  WriteRegStr SHCTX "${IUREDESK_CAPS}" "ApplicationIcon" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
  WriteRegStr SHCTX "${IUREDESK_CAPS}\URLAssociations" "mailto" "IureDesk.mailto"
  WriteRegStr SHCTX "${IUREDESK_CAPS}\FileAssociations" ".eml" "IureDesk.eml"
  WriteRegStr SHCTX "${IUREDESK_CAPS}\FileAssociations" ".ics" "IureDesk.ics"
  WriteRegStr SHCTX "${IUREDESK_CAPS}\FileAssociations" ".vcf" "IureDesk.vcf"
  WriteRegStr SHCTX "Software\RegisteredApplications" "IureDesk" "${IUREDESK_CAPS}"

  ; Avisa al Explorador de que cambiaron las asociaciones.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Section un.IureDeskDefaultApps
  DeleteRegValue SHCTX "Software\RegisteredApplications" "IureDesk"
  DeleteRegKey SHCTX "Software\Iurefficient\IureDesk"
  DeleteRegKey /ifempty SHCTX "Software\Iurefficient"
  DeleteRegKey SHCTX "Software\Classes\IureDesk.mailto"
  DeleteRegKey SHCTX "Software\Classes\IureDesk.eml"
  DeleteRegKey SHCTX "Software\Classes\IureDesk.ics"
  DeleteRegKey SHCTX "Software\Classes\IureDesk.vcf"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd
