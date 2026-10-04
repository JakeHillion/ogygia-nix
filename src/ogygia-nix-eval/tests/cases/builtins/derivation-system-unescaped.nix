map (system: (derivation { name = "d"; inherit system; builder = "/bin/sh"; }).drvPath) [ "\n" "a\"b" "\\" "\t\r" ]
