{ config, pkgs, lib, ... }:
let
  options = {
    debug = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable debug logging";
    };
    local = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "For local (testing) operation - have child processes output to stdout/stderr rather than syslog";
    };
    environment = lib.mkOption {
      type = lib.types.nullOr lib.types.attrs;
      default = null;
    };
    tasks = lib.mkOption {
      description = "Each key is the name of a task, values are converted to task json specifications (see puteron task json spec)";
      default = { };
      type = lib.types.attrsOf (lib.types.mkOptionType {
        name = "json";
        description = "Any JSON";
        check = x: true;
        merge =
          let
            uninit = "UNINIT-nix-puteron-task-merge";
            mergePair = { path, oldVal, newVal }:
              if oldVal == newVal then oldVal
              else if oldVal == uninit then newVal
              else if newVal == uninit then oldVal
              else if builtins.isAttrs oldVal && builtins.isAttrs newVal then
                lib.attrsets.filterAttrs
                  (k: v: v != uninit)
                  (builtins.mapAttrs
                    (k: oldNewVal: mergePair {
                      path = path ++ [ k ];
                      oldVal = builtins.elemAt oldNewVal 0;
                      newVal = builtins.elemAt oldNewVal 1;
                    })
                    (
                      let
                        uninitSuperset =
                          lib.attrsets.genAttrs
                            (lib.lists.unique (builtins.concatLists [ (builtins.attrNames oldVal) (builtins.attrNames newVal) ]))
                            (name: uninit);
                      in
                      lib.attrsets.zipAttrs [ (uninitSuperset // oldVal) (uninitSuperset // newVal) ]
                    ))
              else builtins.throw "Conflicting values in json at ${lib.strings.concatStringsSep "." path}";
          in
          loc: defs: (builtins.foldl'
            (old: new: {
              value = mergePair {
                path = [ ];
                oldVal = old.value;
                newVal = new.value;
              };
            })
            { value = uninit; }
            defs).value;
      });
    };
    controlSystemd = lib.mkOption {
      description = "A map of systemd unit names to control options or null to disable. Where not null, this will create a `long` or `short` puteron task (depending on whether the options indicate it's a oneshot unit) that invokes the wrapper `puteron-control-systemd` command to control the systemd unit from puteron itself (i.e. when started, it'll start the unit; when stopped, it'll stop the unit). The task name will be in the form `systemd-UNIT-UNITSUFFIX`.";
      default = { };
      type = lib.types.attrsOf
        (lib.types.nullOr (lib.types.submodule {
          options = {
            oneshot = lib.mkOption {
              description = "The systemd unit is a oneshot service (changes how child process exits are handled)";
              default = false;
              type = lib.types.bool;
            };
          };
        }));
    };
    listenSystemd = lib.mkOption
      {
        description = "A map of systemd unit names (with dotted suffix) to a boolean. Where true this will create an `empty` puteron task that is turned on and off to match activation of the corresponding systemd unit. Add the task as a weak upstream of other tasks. Only implemented for `.service`, `.mount`, `.target` at this time. The task name will be in the form `systemd-UNIT-UNITSUFFIX`.";
        default = { };
        type = lib.types.attrsOf (lib.types.nullOr lib.types.bool);
      };
  };
  mapSystemdTaskName = name:
    let
      mangled = builtins.replaceStrings [ "." "@" ":" ] [ "-" "-" "-" ] name;
    in
    "systemd-${mangled}";


  # Filtered systemd interop lists
  listenSystemd =
    if options.listenSystemd != null
    then lib.attrsets.mapAttrsToList (k: v: k) (lib.attrsets.filterAttrs (name: value: value) config.puteron.listenSystemd)
    else [ ];
  controlSystemd =
    if options.controlSystemd != null
    then lib.attrsets.filterAttrs (name: value: null != value) config.puteron.controlSystemd
    else { };

  pkg = import ./package.nix {
    pkgs = pkgs;
    debug = config.puteron.debug;
  };

  # Build an options level
  build = { levelName, options, wantedBy }:
    let
      tasks = options.tasks;

      # Build task dir out of tasks
      tasksDir = derivation {
        name = "puteron-${levelName}-tasks-dir";
        system = builtins.currentSystem;
        builder = "${pkgs.python3}/bin/python3";
        args = [
          ./module_gendir.py
          (builtins.toJSON tasks)
        ];
      };

      # Build daemon config
      demonConfig = pkgs.writeTextFile {
        name = "puteron-${levelName}-config";
        text = builtins.toJSON
          ({
            log_type = if config.puteron.local then "stderr" else "syslog";
          } // (builtins.listToAttrs (
            [ ]
              ++ (if options.environment != null then [{
              name = "environment";
              value = options.environment;
            }] else [ ])
              ++ [{
              name = "task_dirs";
              value = [ "${tasksDir}" ];
            }]
          )));
        checkPhase = ''
          ${config.system.build.puteron.pkg}/bin/puteron demon $out --validate
        '';
      };

      systemdHookHelpers = builtins.listToAttrs (map
        (unit:
          let
            mangledName = (builtins.replaceStrings [ "." ] [ "-" ] unit) + "-puteron-hook";
          in
          {
            name = mangledName;
            value = {
              serviceConfig.Type = "oneshot";
              serviceConfig.RemainAfterExit = "yes";
              startLimitIntervalSec = 0;
              serviceConfig.ExecStart = pkgs.writeShellScript "${mangledName}-start" ''
                ${pkg}/bin/puteron on ${mapSystemdTaskName unit}
              '';
              serviceConfig.ExecStop = pkgs.writeShellScript "${mangledName}-stop" ''
                ${pkg}/bin/puteron on ${mapSystemdTaskName unit}
              '';
              wantedBy = [ unit ];
              after = [ unit ];
            };
          })
        listenSystemd);
      script = pkgs.writeShellScript "puteron-${levelName}-script" (lib.concatStringsSep " " ([ ]
        ++ (if config.puteron.debug then [ "RUST_BACKTRACE=full" ] else [ ])
        ++ [ "exec ${pkg}/bin/puteron" "demon" "${demonConfig}" ]
        ++ (if config.puteron.debug then [ "--debug" ] else [ ])
      ));
    in
    {
      script = script;

      systemdServices = { }
        # Root service
        // (
        if config.puteron.enable then {
          puteron = {
            wantedBy = [ wantedBy ];
            serviceConfig.Type = "simple";
            startLimitIntervalSec = 0;
            serviceConfig.Restart = "always";
            serviceConfig.RestartSec = 60;
            script = "exec ${script}";
          };
        } else { }
      )
        // systemdHookHelpers;
    };

  # Generate at root level
  root = build {
    levelName = "root";
    options = config.puteron;
    wantedBy = "multi-user.target";
  };
in
{
  options = {
    puteron = lib.mkOption {
      type = lib.types.submodule {
        options = {
          enable = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = "Enable the puteron service for managing tasks (services). This will create a systemd unit to run puteron, with the task config directory in the Nix store plus an additional directory in `/etc/puteron/tasks` or `~/.config/puteron/tasks`.";
          };
        } // options;
      };
    };
  };

  # Add synthetic tasks here so they can be merged with user overrides
  imports = [
    ({ ... }: {
      config.puteron.tasks = { }

        // (lib.attrsets.listToAttrs (map
        (unit: {
          name = mapSystemdTaskName unit;
          value = {
            type = "empty";
          };
        })
        listenSystemd))

        // (lib.attrsets.mapAttrs'
        (name: value: {
          name = mapSystemdTaskName name;
          value = {
            command = {
              line = [ "${pkg}/bin/puteron-control-systemd" name ];
            };
          } // (if value.oneshot then {
            type = "short";
          } else {
            type = "long";
            started_check = {
              run_path = "puteron-control-systemd-${name}.pid";
            };
          });
        })
        controlSystemd)

        // { };
    })
  ];

  config = {
    system.build.puteron.mapSystemdTaskName = mapSystemdTaskName;
    system.build.puteron.pkg = pkg;

    # Assemble root config
    system.build.puteron.script = root.script;
    systemd.services = root.systemdServices;
  };
}
