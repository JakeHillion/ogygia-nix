let
  a = 1;
in
{
  inherit ({ "f:o" = 1; }) ${f:o};
  inherit ${("a")};
  b = with { "x:y" = 2; }; { inherit ${x:y}; };
  ${(("c"))}.d = 3;
  c.e = 4;
}
