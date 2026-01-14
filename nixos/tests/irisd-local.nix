{ pkgs, lib, system, ogygiaModule }:

pkgs.testers.nixosTest {
  name = "ogygia-irisd-local";

  nodes.machine = { config, pkgs, ... }: {
    imports = [ ogygiaModule ];

    ogygia.irisd = {
      enable = true;
      settings.server.listen = [ "127.0.0.1:35742" ];
    };
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("ogygia-irisd.service")
    machine.wait_for_open_port(35742)

    # Test 1: Verify /nix-cache-info endpoint
    result = machine.succeed("curl -s http://127.0.0.1:35742/nix-cache-info")
    assert "StoreDir: /nix/store" in result, f"Unexpected nix-cache-info: {result}"
    assert "WantMassQuery: 1" in result, f"Missing WantMassQuery in: {result}"
    print("nix-cache-info endpoint works")

    # Test 2: Verify /bloom endpoint returns data
    machine.succeed("curl -sf http://127.0.0.1:35742/bloom -o /tmp/bloom.bin")
    bloom_size = int(machine.succeed("stat -c %s /tmp/bloom.bin").strip())
    assert bloom_size > 4, f"Bloom data too small: {bloom_size} bytes"
    print(f"bloom endpoint returned {bloom_size} bytes")

    # Test 3: Query narinfo for a path that exists in the local /nix/store
    store_path = machine.succeed("readlink -f /run/current-system/sw/bin/bash").strip()
    store_name = store_path.replace("/nix/store/", "")
    store_hash = store_name[:32]

    result = machine.succeed(f"curl -s http://127.0.0.1:35742/{store_hash}.narinfo")
    assert "StorePath:" in result, f"Missing StorePath in narinfo: {result}"
    assert "NarHash:" in result, f"Missing NarHash in narinfo: {result}"
    print(f"narinfo for {store_hash} served from local store")

    # Test 4: HEAD request for narinfo
    machine.succeed(f"curl -f --head http://127.0.0.1:35742/{store_hash}.narinfo")
    print("HEAD request for narinfo works")

    # Test 5: Verify service is running and logs are healthy
    machine.succeed("systemctl is-active ogygia-irisd.service")

    print("All local store tests passed!")
  '';
}
