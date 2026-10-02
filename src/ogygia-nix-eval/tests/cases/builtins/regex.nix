[
  (builtins.match "a(b)?c" "ac")
  (builtins.match "a(b)?c" "abc")
  (builtins.match "a" "ab")
  (builtins.match "([[:alpha:]]+)-([0-9.]+)" "hello-1.2.3")
  (builtins.match ".*\\.nix" "foo.nix")
  (builtins.match "(.*)" "a\nb")
  (builtins.match "[^/]+" "abc")
  (builtins.split "," "a,b,c")
  (builtins.split "(,)" "a,b")
  (builtins.split "([[:space:]]+)" " a  b ")
  (builtins.split "x" "abc")
  (builtins.match "a{2}" "aa")

]
