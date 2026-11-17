DragonSlayer - fan, battery and keyboard control for the MSI GF63 Thin
=======================================================================
An open-source replacement for MSI Dragon Center.
Project page: https://github.com/roskuttan/DragonSlayer

BEFORE YOU INSTALL
  1. Install PawnIO (the signed driver DragonSlayer uses):
     https://github.com/namazso/PawnIO/releases
  2. Tested on the MSI GF63 Thin 10UC. Should work on the GF63 Thin 10UD
     and 10SC. On other laptops it opens read-only and changes nothing.

INSTALL
  Double-click Install.cmd and click Yes on the admin prompt.
  It copies the app to C:\Program Files\DragonSlayer, adds a Start menu
  entry and starts it. After that it opens without any admin prompt.

TRY WITHOUT INSTALLING
  dragonslayer.exe --mock   (demo mode with made-up readings)

UNINSTALL
  Double-click Uninstall.cmd (also copied to C:\Program Files\DragonSlayer).
  The fan goes back to the laptop's own curve.

Windows SmartScreen may warn that the app is from an unknown publisher:
it is not code-signed. Click "More info" -> "Run anyway", or build it
yourself from the source.

Not affiliated with MSI. Use at your own risk. License: MIT (LICENSE.txt);
LpcACPIEC.bin is part of PawnIO, LGPL-2.1 (COPYING-PawnIO.txt).
