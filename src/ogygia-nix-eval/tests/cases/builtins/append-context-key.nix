map
  (
    k:
    builtins.getContext (
      builtins.appendContext "x" {
        ${k} = {
          path = true;
        };
      }
    )
  )
  [
    "/nix/store/p1v2hkzib64izpkmp5zr6gn81rrk8pv1-x/"
    "//nix/store/./p1v2hkzib64izpkmp5zr6gn81rrk8pv1-x/sub/.."
    "/nix/store/p1v2hkzib64izpkmp5zr6gn81rrk8pv1_x"
    "/nix/store/p1v2hkzib64izpkmp5zr6gn81rrk8pv1-.x"
  ]
