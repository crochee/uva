#!/bin/sh
# Render the site configuration, then hand over to nginx.
#
# The image runs as a non-root user with a read-only root
# filesystem, so everything written here lives under /tmp: the
# rendered configuration, nginx's pid file and its temporary paths.
#
# Only the two variables the template names are substituted — nginx's
# own `$variables` (the proxy headers) must survive the rendering.
set -eu

output_dir="${NGINX_ENVSUBST_OUTPUT_DIR:-/tmp/nginx/conf}"
template="${NGINX_TEMPLATE:-/etc/nginx/templates/default.conf.template}"

mkdir -p "$output_dir"
# The single quotes are deliberate: the list names the variables
# `envsubst` replaces, and the shell must not expand it first.
# shellcheck disable=SC2016
envsubst '${UVA_UPSTREAM} ${UVA_PORT}' < "$template" > "$output_dir/default.conf"

exec "$@"
