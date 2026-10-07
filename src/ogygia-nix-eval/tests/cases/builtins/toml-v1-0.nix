map builtins.fromTOML [
  "a = { f = [ 1,\n  2, ] }"
  "a = { f = \"\"\"x\ny\"\"\" }"
  "a = \"\\\\e\\\\x41\""
  "a = '\\e'"
  "a = [ { f = 1 }, ]"
]
