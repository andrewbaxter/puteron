{ config, pkgs, lib, ... }:
let
  options = {
    debug = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable debug logging";
    };
    environment = lib.mkOption {
      type = lib.types.attrset;
      default = null;
    };
    tasks = lib.mkOption {
      description = "Each key is the name of a task, values are converted to task json specifications (see puteron task json spec)";
      type = lib.types.attrsOf (lib.types.oneOf [
        lib.types.str
        lib.types.attrs
      ]);
    };
    controlSystemd = lib.mkOption {
      description = "A map of systemd unit names to control options or null to disable. Where not null, this will create a `long` or `short` puteron task (depending on whether the options indicate it's a oneshot unit) that invokes the wrapper `puteron-control-systemd` command to control the systemd unit from puteron itself (i.e. when started, it'll start the unit; when stopped, it'll stop the unit). The task name will be in the form `systemd-UNIT-UNITSUFFIX`.";
      default = { };
      type = lib.types.attrsOf
        (lib.types.oneOf [
          lib.types.null
          (lib.types.record {
            contents = {
              oneshot = lib.mkOption {
                description = "The systemd unit is a oneshot service (changes how child process exits are handled)";
                default = false;
                type = lib.types.bool;
              };
              exitCode = lib.mkOption
                {
                  description = "The unit code considered as a successful exit (default 0)";
                  default = null;
                  type = lib.types.oneOf [ lib.types.bool lib.types.null ];
                };
            };
          })
        ]);
    };
    listenSystemd = lib.mkOption
      {
        description = "A map of systemd unit names (with dotted suffix) to a boolean. Where true this will create an `empty` puteron task that is turned on and off to match activation of the corresponding systemd unit. Add the task as a weak upstream of other tasks. Only implemented for `.service`, `.mount`, `.target` at this time. The task name will be in the form `systemd-UNIT-UNITSUFFIX`.";
        default = { };
        type = lib.types.attrsOf (lib.types.oneOf [ lib.types.bool lib.types.null ]);
      };
  };
in
{
  options = {
    puteron = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Enable the puteron service for managing tasks (services). This will create a systemd root and user unit to run puteron, with the task config directory in the Nix store plus an additional directory in `/etc/puteron/tasks` or `~/.config/puteron/tasks`.";
      };
      user = options;
    } // options;
  };
  config =
    let
      pkg = import ./package.nix {
        pkgs = pkgs;
        debug = config.puteron.debug;
      };

      # Build an options level
      build = { options, wantedBy }:
        let
          # The `empty` systemd task name is based on the systemd unit name + suffix
          mapSystemdTaskName =
            let mangled = builtins.replaceStrings [ "." "@" ":" ] [ "-" "-" "-" ] name; in "systemd-${mangled}";

          # Defined tasks + generated `empty` tasks for systemd units
          tasks = { }
            // options.tasks
            // (lib.attrsets.mapAttrs'
            (name: value: { name = mapSystemdTaskName name; value = "empty"; })
            (lib.attrsets.filterAttrs (name: value: value != null) options.listenSystemd))
            // (lib.attrsets.mapAttrs'
            (name: value: {
              name = mapSystemdTaskName name;
              value =
                if value.oneshot
                then {
                  "short" = {
                    command = [ ]
                      ++ [ "${pkg}/bin/puteron-control-systemd" "--oneshot" ]
                      ++ (lib.lists.optional (value.exitCode != null) [ "--exit-code" "${builtins.toString value.exitCode}" ]);
                  };
                }
                else {
                  "long" = {
                    command = [ ]
                      ++ [ "${pkg}/bin/puteron-control-systemd" ]
                      ++ (lib.lists.optional (value.exitCode != null) [ "--exit-code" "${builtins.toString value.exitCode}" ]);
                  };
                };
            })
            (lib.attrsets.filterAttrs (name: value: value != null) options.controlSystemd));

          # Build task dir out of tasks
          tasksDir = derivation {
            name = "${name}-tasks-dir";
            system = builtins.currentSystem;
            builder = "${pkgs.python3}/bin/python3";
            args = [
              ./module_gendir.py
              (builtins.toJSON tasks)
            ];
          };

          # Build daemon config
          demonConfig = (pkgs.writeText "${name}-config" (builtins.toJSON (builtins.listToAttrs (
            [ ]
            ++ (lib.lists.optional (environment != null) {
              name = "environment";
              value = environment;
            })
            ++ [{
              name = "task_dirs";
              value = [ tasksDir "%E/puteron/tasks" ];
            }]
          ))));

          # Validate the config - use this as a dep so that the build will fail if validation has errors
          validateConfig = derivation {
            name = "${name}-validate-config";
            system = builtins.currentSystem;
            builder = "${pkgs.bash}/bin/bash";
            args = [
              (pkgs.writeShellScript "${name}-validate-config-run" ''
                set -xeu
                ${config.system.build.puteron_pkg}/bin/puteron demon run ${demonConfig} --validate
                touch $out
              '')
            ];
          };

          # Build hooks for systemd services hooked with `empty` tasks 
          buildSystemdAddons = type:
            let
              suffix = ".${type}";
            in
            (lib.attrsets.mapAttrs'
              (name: value: {
                name = lib.strings.removeSuffix suffix name;
                value =
                  {
                    serviceConfig.ExecStartPost = "${pkg}/bin/puteron on ${listenSystemdTask name}";
                    serviceConfig.ExecStopPre = "${pkg}/bin/puteron off ${listenSystemdTask name}";
                  };
              })
              (lib.attrsets.filterAttrs
                (name: value: lib.strings.hasSuffix suffix name)
                listenSystemd));
        in
        {
          script = assert "${validateConfig}" != "";
            lib.concatStringsSep " " ([ ]
              ++ [ "${pkg}/bin/puteron" "demon" "run" "${demonConfig}" ]
              ++ (lib.lists.optional config.puteron.debug "--debug")
            );

          systemdServices = { }
            # Root service
            // (lib.mkIf config.puteron.enable {
            puteron = {
              wantedBy = [ wantedBy ];
              serviceConfig.Type = "simple";
              startLimitIntervalSec = 0;
              serviceConfig.Restart = "on-failure";
              serviceConfig.RestartSec = 60;
              script = config.system.build.puteronScript;
            };
          })
            # Apply `empty` task hooks
            // buildSystemdAddons "service";
          # Apply `empty` task hooks
          systemdTargets = buildSystemdAddons "target";
          systemdMounts = buildSystemdAddons "mount";
        };

      # Generate at root + user levels
      root = build {
        options = config.puteron;
        wantedBy = "multi-user.target";
      };
      user = build {
        options = config.puteron.user;
        wantedBy = "default.target";
      };
    in
    {
      system.build.puteronPkg = pkg;

      # Assemble root config
      system.build.puteronScript = root.script;
      systemd.services = root.systemdServices;
      systemd.targets = root.systemdTargets;

      # Assemble user config
      system.build.puteronUserScript = user.script;
      systemd.user.services = user.systemdServices;
      systemd.user.targets = user.systemdTargets;
    };
}
