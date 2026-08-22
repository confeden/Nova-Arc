; Explorer's context menu, added on install and removed on uninstall.
;
; A CASCADING menu, built entirely from the registry: one "Nova Prism" entry
; that opens into verbs. An empty `SubCommands` is what makes it cascade
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
; THE CASCADE FLAG IS AN EMPTY `SubCommands`, and it took three attempts to
; land on that. The first two rendered one FLAT "Nova Prism" item; the first
; also answered a click with "This file does not have an app associated with it
; for performing this action":
;
;   1. ExtendedSubCommandsKey = "Software\Classes\NovaPrism.Pack". That value
;      is resolved relative to HKEY_CLASSES_ROOT, not to the hive the verb was
;      written into, so the shell looked in HKCR\Software\Classes\NovaPrism.Pack
;      — nowhere. No submenu and no command: hence the error on click.
;   2. ExtendedSubCommandsKey = "NovaPrism.Pack", then
;      "<root>\shell\NovaPrism" with the children moved into the verb's own
;      `shell` subkey, copying Windows' own efscore.dll entry. Both resolve.
;      Both still flat.
;
; Microsoft documents TWO separate mechanisms, and mixing them is the trap:
; `ExtendedSubCommandsKey` names a different key that holds the children, while
; an EMPTY `SubCommands` says "the children are in my own `shell` subkey". Only
; the second one renders here. Two more requirements come with it, and both are
; met above: the parent must have NO command, and its default value must be
; ABSENT — not empty, absent, which is why nothing here ever writes it.
;
; The children living under each root means every root carries its own copy.
; Four extra keys, and no indirection left to get wrong.
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

; A cascading parent: a label, no command of its own, no default value, and an
; EMPTY `SubCommands`. That empty string is the whole flag — it tells the shell
; "this verb is a cascade, read its children out of my own `shell` subkey".
; `ExtendedSubCommandsKey` is the OTHER mechanism, the one that names a
; separate key, and on Windows 11 it did not render a submenu at any of the
; three paths tried. Do not put it back alongside this.
!macro NOVA_MENU_ROOT KEY
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "MUIVerb" "Nova Prism"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "SubCommands" ""
  ; An upgrade from either broken layout must not leave this behind: two
  ; cascade mechanisms on one verb is not a documented combination.
  DeleteRegValue SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "ExtendedSubCommandsKey"
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
