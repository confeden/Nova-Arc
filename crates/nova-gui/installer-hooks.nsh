; Explorer's context menu, added on install and removed on uninstall.
;
; A CASCADING menu, built entirely from the registry: one "Nova Prism" entry
; that opens into verbs. `ExtendedSubCommandsKey` is what makes it cascade
; without a COM handler — the alternative is an IExplorerCommand DLL, which is
; also the only way onto Windows 11's *short* menu. Here the entry lands in the
; short menu on Windows 10 and under "Show more options" on Windows 11, which
; is where 7-Zip and WinRAR live too until they ship a packaged handler.
;
; Everything is written to SHCTX, the root Tauri's own installer already chose:
; HKLM for a machine-wide install, HKCU for a per-user one. Writing HKLM
; unconditionally would need admin rights the per-user path does not have.
;
; ---------------------------------------------------------------------------
; THE SHAPE HERE IS COPIED FROM WINDOWS' OWN WORKING CASCADE, and it took two
; wrong attempts to get here, both of which rendered as one FLAT "Nova Prism"
; item that answered a click with "This file does not have an app associated
; with it for performing this action":
;
;   1. ExtendedSubCommandsKey = "Software\Classes\NovaPrism.Pack". The value is
;      resolved relative to HKEY_CLASSES_ROOT, not to the hive the verb was
;      written into, so the shell went looking in
;      HKCR\Software\Classes\NovaPrism.Pack — nowhere.
;   2. ExtendedSubCommandsKey = "NovaPrism.Pack", which DOES resolve. Still
;      flat.
;
; The one cascade on a stock Windows 11 that actually works —
; Directory\shell\UpdateEncryptionSettings, from efscore.dll — does it like
; this: the value points at the VERB'S OWN KEY, and the children live in that
; key's own `shell` subkey. No separate shared key, no indirection. Copied
; verbatim in shape; the cost is that each root carries its own copy of the
; child verbs, which is four extra keys and no ambiguity.
;
; TWO MENUS, because the verbs differ:
;   * and Directory  -> "Сжать" (anything can be compressed)
;   archive types    -> open / extract / test, and "Сжать" as well
;
; The archive menu is attached to nova's own ProgID and to
; SystemFileAssociations for zip, 7z and rar — that last key adds a verb to a
; file type WITHOUT taking its association away, so whatever opens .zip today
; keeps opening it.
;
; Both menus use the same verb NAME, `NovaPrism`, deliberately: a .zip matches
; both `*` and SystemFileAssociations\.zip, and two differently-named verbs
; would put two "Nova Prism" entries in one menu. Same name means the shell
; shows one of them — and since the archive menu also carries "Сжать", either
; winner is a working menu rather than half of one.
; ---------------------------------------------------------------------------

; A cascading parent: no command of its own, a label, and a pointer back to
; itself so the shell reads the children out of its own `shell` subkey.
!macro NOVA_MENU_ROOT KEY
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "MUIVerb" "Nova Prism"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "ExtendedSubCommandsKey" "${KEY}\shell\NovaPrism"
!macroend

!macro NOVA_MENU_ITEM KEY ORDER LABEL ARGS
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism\shell\${ORDER}" "MUIVerb" "${LABEL}"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism\shell\${ORDER}" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism\shell\${ORDER}\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" ${ARGS} "%1"'
!macroend

; Anything that is not one of our archive types: compress only.
!macro NOVA_PACK_MENU KEY
  !insertmacro NOVA_MENU_ROOT "${KEY}"
  !insertmacro NOVA_MENU_ITEM "${KEY}" "10pack" "Сжать в Nova Prism" "--compress"
!macroend

; An archive: open first, because it is the one people reach for; test last,
; because it is the rare one; compress at the end, since an archive can be put
; inside another one and 7-Zip offers the same.
!macro NOVA_ARCHIVE_MENU KEY
  !insertmacro NOVA_MENU_ROOT "${KEY}"
  !insertmacro NOVA_MENU_ITEM "${KEY}" "10open" "Открыть" ""
  !insertmacro NOVA_MENU_ITEM "${KEY}" "20into" "Распаковать в отдельную папку" "--extract-into"
  !insertmacro NOVA_MENU_ITEM "${KEY}" "30here" "Распаковать здесь" "--extract-here"
  !insertmacro NOVA_MENU_ITEM "${KEY}" "40test" "Проверить" "--test"
  !insertmacro NOVA_MENU_ITEM "${KEY}" "50pack" "Сжать в Nova Prism" "--compress"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Left over from the two broken layouts: an upgrade has to clear them, or a
  ; stale ExtendedSubCommandsKey target keeps answering.
  DeleteRegKey SHCTX "Software\Classes\NovaPrism.Pack"
  DeleteRegKey SHCTX "Software\Classes\NovaPrism.Archive"

  ; --- what can be compressed: any file, any folder
  !insertmacro NOVA_PACK_MENU "*"
  !insertmacro NOVA_PACK_MENU "Directory"

  ; --- what can be opened: our own archives and the foreign ones we read
  !insertmacro NOVA_ARCHIVE_MENU "NovaPrismArchive"
  !insertmacro NOVA_ARCHIVE_MENU "SystemFileAssociations\.zip"
  !insertmacro NOVA_ARCHIVE_MENU "SystemFileAssociations\.7z"
  !insertmacro NOVA_ARCHIVE_MENU "SystemFileAssociations\.rar"

  ; Explorer caches the menu; without this the entry appears only after a
  ; restart, which reads as "the installer did not work".
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Deleting the verb key takes its `shell` subtree with it.
  DeleteRegKey SHCTX "Software\Classes\*\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\Directory\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\NovaPrismArchive\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\SystemFileAssociations\.zip\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\SystemFileAssociations\.7z\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\SystemFileAssociations\.rar\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\NovaPrism.Pack"
  DeleteRegKey SHCTX "Software\Classes\NovaPrism.Archive"
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
!macroend
