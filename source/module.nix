{ config, pkgs, lib, ... }:
{
  options = {
    puteron =
      let
        submoduleEnum = spec: lib.types.addCheck
          (lib.types.submodule { options = spec; })
          (v: builtins.length (lib.attrsToList v) == 1);
        upstreamArg = lib.mkOption {
          default = null;
          type = lib.types.nullOr (lib.types.attrsOf (lib.types.enum [ "strong" "weak" ]));
        };
        defaultOnArg = lib.mkOption {
          default = null;
          type = lib.types.nullOr lib.types.bool;
        };
        envArg = lib.mkOption {
          default = null;
          type = lib.types.nullOr (lib.types.submodule {
            options = {
              clear = lib.mkOption {
                default = null;
                type = lib.types.nullOr (lib.types.attrsOf lib.types.bool);
              };
              add = lib.mkOption {
                default = null;
                type = lib.types.nullOr (lib.types.attrsOf lib.types.str);
              };
            };
          });
        };
        simpleDurationType = lib.types.strMatching "\\d+[hms]";
        durationArg = lib.mkOption {
          default = null;
          type = lib.types.nullOr simpleDurationType;
          description = "Like 10s or 5m";
        };
        commandArg = lib.mkOption {
          type = lib.types.submodule {
            options = {
              working_directory = lib.mkOption {
                default = null;
                type = lib.types.nullOr lib.types.str;
              };
              environment = envArg;
              command = lib.mkOption {
                type = lib.types.listOf lib.types.str;
              };
            };
          };
        };
      in
      {
        enable = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Enable the puteron service for managing puteron services (tasks)";
        };
        debug = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Enable debug logging";
        };
        environment = envArg;
        tasks = lib.mkOption {
          description = "See puteron documentation for field details";
          type = lib.types.attrsOf (submoduleEnum {
            empty = lib.mkOption {
              default = null;
              type = lib.types.nullOr (lib.types.submodule {
                options = {
                  upstream = upstreamArg;
                  default_on = defaultOnArg;
                };
              });
            };
            perpetual = lib.mkOption {
              default = null;
              type = lib.types.nullOr (lib.types.submodule {
                options = {
                  upstream = upstreamArg;
                  default_on = defaultOnArg;
                  command = commandArg;
                  started_check = lib.mkOption {
                    default = null;
                    type = lib.types.nullOr (submoduleEnum {
                      tcp_socket = lib.mkOption {
                        default = null;
                        type = lib.types.nullOr lib.types.str;
                      };
                      path = lib.mkOption {
                        default = null;
                        type = lib.types.nullOr lib.types.str;
                      };
                    });
                  };
                  restart_delay = durationArg;
                  stop_timeout = durationArg;
                };
              });
            };
            finite = lib.mkOption {
              default = null;
              type = lib.types.nullOr (lib.types.submodule {
                options = {
                  upstream = upstreamArg;
                  default_on = defaultOnArg;
                  command = commandArg;
                  success_codes = lib.mkOption {
                    default = null;
                    type = lib.types.nullOr (lib.types.listOf lib.types.int);
                  };
                  started_action = lib.mkOption {
                    default = null;
                    type = lib.types.nullOr lib.types.enum [ "turn_off" "delete" ];
                  };
                  restart_delay = durationArg;
                  stop_timeout = durationArg;
                };
              });
            };
            external = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
            };
          });
        };
      };
  };
  config = {
    system.build.puteron_pkg = import ./package.nix {
      pkgs = pkgs;
      debug = config.puteron.debug;
    };
    system.build.puteron_script = pkgs.writeShellScript "puteron-run" (
      let
        tasks = builtins.listToAttrs (map
          (t: {
            name = t.name;
            value = lib.attrsets.filterAttrsRecursive (k: v: v != null) t.value;
          })
          (lib.attrsToList config.puteron.tasks));
        taskDirs = derivation {
          name = "puteron-task-configs";
          system = builtins.currentSystem;
          builder = "${pkgs.python3}/bin/python3";
          args = [
            ./module_gendir.py
            (builtins.toJSON tasks)
          ];
        };
      in
      lib.concatStringsSep " " (
        [
          "${config.system.build.puteron_pkg}/bin/puteron"
          "demon"
          "run"
          (pkgs.writeText "puteron-config" (builtins.toJSON (builtins.listToAttrs (
            [ ]
            ++ (lib.lists.optional (config.puteron.environment != null) {
              name = "environment";
              value = config.puteron.environment;
            })
            ++ [{
              name = "task_dirs";
              value = [ taskDirs ];
            }]
          ))))
        ]
        ++ (lib.lists.optional config.puteron.debug "--debug")
      )
    );
    systemd.services = lib.mkIf config.puteron.enable {
      puteron = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.Type = "simple";
        startLimitIntervalSec = 0;
        serviceConfig.Restart = "on-failure";
        serviceConfig.RestartSec = 60;
        script = config.system.build.puteron_script;
      };
    };
  };
}
