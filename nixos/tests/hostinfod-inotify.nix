{ pkgs, lib, system, ogygiaModule }:

# This test verifies that ogygia-hostinfod correctly detects filesystem changes
# and updates etcd. We test this by:
# 1. Starting the service and verifying initial etcd values
# 2. The daemon watches system paths for changes
# 3. We verify the service is running and functioning
#
# Note: We don't test symlink changes to /run/current-system in the VM test
# because that would break the running system. The inotify functionality is
# tested separately with unit tests or manual testing.

pkgs.testers.nixosTest {
  name = "ogygia-hostinfod";

  nodes.machine = { config, pkgs, ... }: {
    imports = [ ogygiaModule ];

    # Set up etcd
    services.etcd = {
      enable = true;
      listenClientUrls = [ "http://localhost:2379" ];
      advertiseClientUrls = [ "http://localhost:2379" ];
    };

    # Configure ogygia with etcd - hostinfod should auto-enable
    ogygia = {
      enable = true;
      domain = "test.local";
      etcd.endpoints = [ "http://localhost:2379" ];
    };

    # Set a custom hostname for predictable testing
    networking.hostName = "test-host";
  };

  testScript = ''
    import time

    machine.start()
    
    # Wait for etcd to be ready
    machine.wait_for_unit("etcd.service")
    machine.wait_for_open_port(2379)
    
    # Wait for hostinfod to start
    machine.wait_for_unit("ogygia-hostinfod.service")
    
    # Wait a moment for initial etcd write
    time.sleep(3)
    
    # Check that initial values are in etcd
    hostname = "test-host"
    
    # Test current system - should have a value (even if "unknown")
    result = machine.succeed("etcdctl get /ogygia/nixos/versions/{}/current --print-value-only".format(hostname))
    current_revision = result.strip()
    print(f"Current revision in etcd: {current_revision}")
    assert len(current_revision) > 0, "No current revision found in etcd"
    
    # Test booted system
    result = machine.succeed("etcdctl get /ogygia/nixos/versions/{}/booted --print-value-only".format(hostname))
    booted_revision = result.strip()
    print(f"Booted revision in etcd: {booted_revision}")
    assert len(booted_revision) > 0, "No booted revision found in etcd"
    
    # Verify the service is still running after initial setup
    machine.succeed("systemctl is-active ogygia-hostinfod.service")
    print("Service is still running")
    
    # Check that the service is watching for changes
    # We can verify this by checking the logs
    logs = machine.succeed("journalctl -u ogygia-hostinfod.service --no-pager -n 10")
    print(f"Service logs: {logs}")
    
    # Verify that the watching is set up correctly
    assert "Watching /run for current" in logs, "Service not watching /run for current"
    assert "Watching /run for booted" in logs, "Service not watching /run for booted"
    assert "Watching /nix/var/nix/profiles for nextboot" in logs, "Service not watching profiles"
    
    print("SUCCESS: ogygia-hostinfod is running and properly configured!")
    print("Note: Full inotify testing with system changes should be done manually")
    print("      or in a separate integration test environment.")
  '';
}
