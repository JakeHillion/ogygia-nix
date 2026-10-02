[
  (builtins.seq 1 2)
  (builtins.deepSeq [ 1 ] 2)
  (builtins.tryEval (throw "x"))
  (builtins.tryEval (assert false; 1))
  (builtins.tryEval 1)
  (builtins.add 1 2)
  (builtins.sub 1 2.5)
  (builtins.mul 3 4)
  (builtins.div 7 2)
  (builtins.bitAnd 12 10)
  (builtins.bitOr 12 10)
  (builtins.bitXor 12 10)
  (builtins.ceil 1.5)
  (builtins.floor (-1.5))
  (builtins.lessThan 1 2)
  (builtins.functionArgs ({ a, b ? 2, ... }: a))
  (builtins.functionArgs (x: x))
  builtins.langVersion
  builtins.nixVersion
  builtins.storeDir
  (builtins.addErrorContext "ctx" 5)
]
