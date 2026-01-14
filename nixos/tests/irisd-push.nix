{ pkgs, lib, system, ogygiaModule, ogygia }:

pkgs.testers.nixosTest {
  name = "ogygia-irisd-push";

  nodes.machine = { config, pkgs, ... }: {
    imports = [ ogygiaModule ];

    ogygia.irisd = {
      enable = true;
      settings.server.listen = [ "127.0.0.1:35742" ];
    };

    environment.systemPackages = [ ogygia ];

    # Enable nix-command for nix key generate-secret and nix store sign
    nix.settings.experimental-features = [ "nix-command" ];
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("ogygia-irisd.service")
    machine.wait_for_open_port(35742)

    # Generate a signing key
    machine.succeed("nix key generate-secret --key-name test-cache > /tmp/signing-key")
    print("Generated signing key")

    # Build a test derivation (creates an ultimate path)
    store_path = machine.succeed(
      "nix-build --no-out-link -E 'derivation { name = \"test-push\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [\"-c\" \"echo test > $out\"]; }'"
    ).strip()
    print(f"Built test derivation: {store_path}")

    # Verify the path is ultimate (locally built)
    path_info = machine.succeed(f"nix path-info --json {store_path}")
    assert '"ultimate":true' in path_info, f"Path should be ultimate: {path_info}"
    print("Verified path is ultimate")

    # Push to irisd
    machine.succeed(
      f"echo {store_path} | ogygia iris push --signing-key /tmp/signing-key --no-closure"
    )
    print("Push completed successfully")

    # Extract hash from store path
    store_name = store_path.replace("/nix/store/", "")
    store_hash = store_name[:32]
    print(f"Store hash: {store_hash}")

    # Verify narinfo has our signature
    narinfo = machine.succeed(f"curl -s http://127.0.0.1:35742/{store_hash}.narinfo")
    print(f"Narinfo content:\n{narinfo}")

    assert "Sig: test-cache:" in narinfo, f"Expected signature from test-cache in narinfo: {narinfo}"
    print("Verified narinfo has test-cache signature")

    # Verify we can still get the narinfo on subsequent requests
    narinfo2 = machine.succeed(f"curl -s http://127.0.0.1:35742/{store_hash}.narinfo")
    assert "Sig: test-cache:" in narinfo2, "Signature should persist on subsequent requests"

    print("All push tests passed!")
  '';
}
