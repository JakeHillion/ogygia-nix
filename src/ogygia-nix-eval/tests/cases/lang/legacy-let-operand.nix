[
  let { body = { x = 1; }; }.x
  let { body = { }; }.x or 2
  (let { body = f; f = x: x; } 3)
  (let /* c */ { body = 1; } + 1)
  let { body = "${let { body = "a"; }}"; }
  let{body=1;}let{body=2;}
  let { ${"body"} = { a = { }; }; }.a
]
