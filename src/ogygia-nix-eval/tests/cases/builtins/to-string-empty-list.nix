[
  (toString [ [ ] 1 ])
  (toString [ 1 [ ] 2 ])
  (toString [ 1 [ [ ] ] 2 ])
  (toString [ [ ] [ ] 1 [ ] ])
  (toString [ null 1 [ 2 [ ] [ null 1 ] ] true ])
  "${toString [ (builtins.genList (x: x) 0) "a" ]}"
]
