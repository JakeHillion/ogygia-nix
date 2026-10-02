[ ({ a = throw "no"; } ? a) ({ a.b = throw "no"; } ? a.b) ({ a = { b = 1; }; } ? a.c) ]
