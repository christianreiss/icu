ICU @VERSION@ für Windows 10/11 (64 Bit)

Entpacken, icu.exe starten. Keine Installation, keine Administratorrechte.

Beim ersten Start warnt Windows („Der Computer wurde durch Windows
geschützt“), weil icu.exe nicht signiert ist: „Weitere Informationen“, dann
„Trotzdem ausführen“.

ICU trägt sich beim Start für .icu-Dateien ein (Einladungen und Passports,
per Doppelklick) und legt seine Daten ab unter:
  %APPDATA%\icu\config        Einstellungen und Identität
  %LOCALAPPDATA%\icu\icu.log  Protokoll des letzten Starts

Der private Schlüssel ist mit dem Windows-Konto verschlüsselt (DPAPI): auf
einen anderen Rechner oder in ein anderes Konto zieht ICU per Passport um,
nicht durch Kopieren des Ordners.

Windows N/KN ohne „Media Feature Pack“: ICU startet nicht (Kamera braucht
Media Foundation).

Deinstallieren: icu.exe löschen, die Ordner oben löschen, und unter
HKEY_CURRENT_USER\Software\Classes die Schlüssel .icu, ICU.Invite und
AppUserModelId\ICU entfernen.
