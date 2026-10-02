rec { a = 1; b = a + 1; c = { d = b; }; inherit (c) d; }
