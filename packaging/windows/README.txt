ICU @VERSION@ für Windows 10/11 (64 Bit)

Entpacken, icu.exe starten. Keine Installation, keine Administratorrechte.

Einen Eintrag im Startmenü legt ICU ohne Installation nicht an. Wer ihn
möchte: icu.exe dorthin legen, wo es bleiben soll, dann Rechtsklick auf
icu.exe → „An „Start“ anheften“.

Beim ersten Start warnt Windows („Der Computer wurde durch Windows
geschützt“), weil icu.exe nicht signiert ist: „Weitere Informationen“, dann
„Trotzdem ausführen“. Ist die Intelligente App-Steuerung (Smart App Control)
eingeschaltet, blockiert sie die unsignierte icu.exe; eine Ausnahme für
einzelne Apps kennt sie nicht, „Trotzdem ausführen“ gibt es dort nicht.

ICU trägt sich beim Start für .icu-Dateien ein (Einladungen und Passports,
per Doppelklick) und legt seine Daten ab unter:
  %APPDATA%\icu\config        Einstellungen und Identität
  %LOCALAPPDATA%\icu\data\icu.log  Protokoll (ab 1 MiB nach icu.log.1)

Der private Schlüssel ist mit dem Windows-Konto verschlüsselt (DPAPI): auf
einen anderen Rechner oder in ein anderes Konto zieht ICU per Passport um,
nicht durch Kopieren des Ordners. Ein Passport enthält den privaten
Schlüssel: wer ihn hat, meldet sich mit deinem Zugang an. Wie ein
Passwort behandeln.

Windows N/KN ohne „Media Feature Pack“: ICU startet nicht (Kamera braucht
Media Foundation).

Deinstallieren: icu.exe löschen, die Ordner oben löschen, und unter
HKEY_CURRENT_USER\Software\Classes die Schlüssel .icu, ICU.Invite und
AppUserModelId\ICU entfernen.
