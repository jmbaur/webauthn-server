{ config, lib, pkgs, ... }:
let
  cfg = config.services.webauthn-tiny;
in
{
  options = {
    services.webauthn-tiny = {
      enable = lib.mkEnableOption "webauthn-tiny server";
      environmentFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        description = ''
          Path to a file containing a secret that will be used to sign web
          sessions from the server.
        '';
      };
      relyingParty = {
        id = lib.mkOption {
          type = lib.types.str;
          description = ''
            An ID that corresponds to the domain applicable for that Relying
            Party.
          '';
          example = "mywebsite.com";
        };
        origin = lib.mkOption {
          type = lib.types.str;
          description = ''
            The origin on which registrations for the Relying Party will take
            place.
          '';
          example = "https://mywebsite.com";
        };
      };
      nginx = {
        enable = lib.mkEnableOption "nginx support";
        virtualHost = lib.mkOption {
          type = lib.types.str;
          description = ''
            The virtual host that this service will serve on.
          '';
        };
        basicAuth = lib.mkOption {
          type = lib.types.attrsOf lib.types.str;
          description = ''
            A static mapping of usernames to passwords. WARNING: only use this
            for testing purposes.
          '';
          default = { };
          example = { myuser = "mypassword"; };
        };
        basicAuthFile = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          description = ''
            A path to an htpasswd file.
          '';
          default = null;
        };
        protectedVirtualHosts = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          description = ''
            A list of virtual hosts that will be protected by this webauthn
            server. This uses nginx's auth_request functionality.
          '';
          default = [ ];
        };
        enableACME = lib.mkEnableOption "enable ACME on this virtual host";
        useACMEHost = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Whether to ask Let's Encrypt to sign a certificate for this vhost.
            Alternately, you can use an existing certificate through
            useACMEHost.
          '';
        };
      };
    };
  };
  config = lib.mkIf cfg.enable {
    services.nginx = lib.mkIf cfg.nginx.enable {
      enable = true;
      virtualHosts = lib.genAttrs cfg.nginx.protectedVirtualHosts
        (_: {
          extraConfig = ''
            auth_request /auth;
            error_page 401 = @error401;
          '';
          locations."= /auth" = {
            proxyPass = "http://[::1]:8080/api/validate";
            extraConfig = ''
              proxy_pass_request_body off;
              proxy_set_header Content-Length "";
              proxy_set_header X-Original-URI $request_uri;
            '';
          };
          locations."@error401".return = "302 https://${cfg.nginx.virtualHost}/?url=https://$http_host&request_uri";
        }) // {
        ${cfg.nginx.virtualHost} =
          let
            withProxy = { extraConfig ? "", ... }@args: args // {
              proxyPass = "http://[::1]:8080";
              extraConfig = extraConfig + ''
                proxy_set_header Host            $host;
                proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
                proxy_set_header X-Remote-User   $remote_user;
              '';
            };
          in
          {
            inherit (cfg.nginx) enableACME useACMEHost;
            forceSSL = true; # webauthn is only available over HTTPS
            locations."= /api/validate" = withProxy { };
            locations."/api" = withProxy {
              inherit (cfg.nginx) basicAuthFile basicAuth;
            };
            locations."/" = {
              root = "${pkgs.webauthn-tiny-ui}";
              tryFiles = "$uri /index.html =404";
            };
          };
      };
    };

    systemd.services.webauthn-tiny = {
      enable = true;
      description = "webauthn-tiny (https://github.com/jmbaur/webauthn-tiny)";
      environment.WEBAUTHN_TINY_LOG = "info";
      serviceConfig = {
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        StateDirectory = "webauthn-tiny";
        ProtectSystem = true;
        ProtectHome = true;
        DynamicUser = true;
        ExecStart = "${pkgs.webauthn-tiny}/bin/webauthn-tiny " +
          lib.escapeShellArgs [
            "--id=${cfg.relyingParty.id}"
            "--origin=${cfg.relyingParty.origin}"
            "--session-secret-file=${cfg.sessionSecretFile}"
          ];
      };
      wantedBy = [ "multi-user.target" ];
    };
  };
}
