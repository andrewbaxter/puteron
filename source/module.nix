{ config, pkgs, lib, ... }:
{
  options = {
    puteron =
      let
        submodule = spec: lib.types.submodule { options = spec; };
        submoduleEnum = spec: lib.types.addCheck
          (lib.types.submodule {
            options = lib.listToAttrs
              (map
                (e: {
                  name = e.name;
                  value = lib.mkOption {
                    default = null;
                    type = lib.types.nullOr e.value;
                  };
                })
                (lib.attrsToList spec));
          })
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
          type = lib.types.nullOr (submodule {
            keep_all = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.bool;
            };
            keep = lib.mkOption {
              default = null;
              type = lib.types.nullOr (lib.types.attrsOf lib.types.bool);
            };
            add = lib.mkOption {
              default = null;
              type = lib.types.nullOr (lib.types.attrsOf lib.types.str);
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
          type = submodule {
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
            external = lib.types.str;
            empty = submodule {
              upstream = upstreamArg;
              default_on = defaultOnArg;
            };
            long = submodule {
              upstream = upstreamArg;
              default_on = defaultOnArg;
              command = commandArg;
              started_check = lib.mkOption {
                default = null;
                type = lib.types.nullOr (submoduleEnum {
                  tcp_socket = lib.types.str;
                  path = lib.types.str;
                });
              };
              restart_delay = durationArg;
              stop_timeout = durationArg;
            };
            short = submodule {
              upstream = upstreamArg;
              default_on = defaultOnArg;
              schedule = lib.mkOption {
                default = null;
                type = lib.types.listOf (submoduleEnum {
                  period = submodule {
                    period = lib.mkOption {
                      type = lib.types.str;
                    };
                    scattered = lib.mkOption {
                      default = null;
                      type = lib.types.nullOr lib.types.bool;
                    };
                  };
                  hourly = lib.types.str;
                  daily = lib.types.str;
                  weekly = submodule {
                    weekday = lib.mkOption {
                      type = lib.types.str;
                    };
                    time = lib.mkOption {
                      type = lib.types.str;
                    };
                  };
                  monthly = submodule {
                    day = lib.mkOption {
                      type = lib.types.int;
                    };
                    time = lib.mkOption {
                      type = lib.types.str;
                    };
                  };
                  yearly = submodule {
                    month = lib.mkOption {
                      type = lib.types.str;
                    };
                    day = lib.mkOption {
                      type = lib.types.int;
                    };
                    time = lib.mkOption {
                      type = lib.types.str;
                    };
                  };
                });
              };
              command = commandArg;
              success_codes = lib.mkOption {
                default = null;
                type = lib.types.nullOr (lib.types.listOf lib.types.int);
              };
              started_action = lib.mkOption {
                default = null;
                type = lib.types.nullOr (lib.types.enum [ "turn_off" "delete" ]);
              };
              restart_delay = durationArg;
              stop_timeout = durationArg;
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
        removeAttrsNull = v:
          if builtins.isAttrs v then
            lib.listToAttrs
              (map
                (e: { name = e.name; value = removeAttrsNull e.value; })
                (builtins.filter
                  (e: e.value != null)
                  (lib.attrsToList v)
                )
              )
          else if builtins.isList v then map removeAttrsNull v
          else v;
        tasks = removeAttrsNull config.puteron.tasks;
        taskDirs = derivation {
          name = "puteron-task-configs";
          system = builtins.currentSystem;
          builder = "${pkgs.python3}/bin/python3";
          args = [
            ./module_gendir.py
            (builtins.toJSON tasks)
          ];
        };
        demon = (pkgs.writeText "puteron-config" (builtins.toJSON (removeAttrsNull (builtins.listToAttrs (
          [ ]
          ++ (lib.lists.optional (config.puteron.environment != null) {
            name = "environment";
            value = config.puteron.environment;
          })
          ++ [{
            name = "task_dirs";
            value = [ taskDirs ];
          }]
        )))));
      in
      lib.concatStringsSep " " (
        [ "${config.system.build.puteron_pkg}/bin/puteron" "demon" "run" "${demon}" ]
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
