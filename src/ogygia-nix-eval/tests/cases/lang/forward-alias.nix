[ (rec { a = b; b = 1; }) (let a = b; b = 2; in a) (let a = b; b = c; c = 3; in [ a b c ]) ]
