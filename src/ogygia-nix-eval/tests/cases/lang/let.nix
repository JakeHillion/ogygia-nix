let a = 1; b = a + c; c = 2; inherit (x) y; x = { y = 3; }; in [ a b c y ]
