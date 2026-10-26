/**
 * The deployment's plugin composition, rendered for an operator.
 *
 * Two halves, and the skipped half is the reason the panel exists:
 * a plugin the operator configured but the server could not install
 * is silently absent from every other surface — the plugin's tools
 * never appear under Tools, and the session that "should" have the
 * capability simply does not. This panel is the only place the
 * reason is shown, so it is always visible when non-empty: no
 * disclosure triangle, and the count sits in the section header.
 *
 * It renders the deployment-wide record inside a session page on
 * purpose: the question an operator asks here is "what does this
 * server have?", and the answer does not vary by session.
 */

import { useEffect, useState } from 'react';
import { pluginsApi, type PluginsResponse } from '../../api/plugins';
import { EmptyState } from '../ui/EmptyState';
import { SkeletonList } from '../ui/SkeletonList';
import { Section, StatusPill } from '../ui/visual';
import './DeploymentPlugins.css';

export function DeploymentPlugins() {
  const [composition, setComposition] = useState<PluginsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    pluginsApi
      .list(controller.signal)
      .then((data) => {
        setComposition(data);
        setError(null);
      })
      .catch((e: Error) => {
        if (e.name === 'AbortError') return;
        // The composition is secondary information on this page: a
        // failed fetch degrades to its own message rather than taking
        // the session down with it.
        setError(e.message);
      });
    return () => controller.abort();
  }, []);

  const resolved = composition?.resolved ?? [];
  const skipped = composition?.skipped ?? [];

  return (
    <Section
      title="Deployment plugins"
      glyph="🔌"
      meta={
        composition ? (
          <>
            <StatusPill
              tone={resolved.length > 0 ? 'green' : 'ghost'}
              testId="deployment-plugins-installed-count"
            >
              {`${resolved.length} installed`}
            </StatusPill>
            {skipped.length > 0 && (
              <StatusPill tone="red" testId="deployment-plugins-skipped-count">
                {`${skipped.length} skipped`}
              </StatusPill>
            )}
          </>
        ) : null
      }
      testId="deployment-plugins"
    >
      {error && (
        <p className="nt-plugins__error" role="alert" data-testid="deployment-plugins-error">
          {`Could not read the plugin composition: ${error}`}
        </p>
      )}

      {!error && !composition && <SkeletonList count={1} testId="deployment-plugins-skeleton" />}

      {composition && resolved.length === 0 && skipped.length === 0 && (
        <EmptyState
          icon="🔌"
          title="No plugins configured"
          description="This deployment runs the surface compiled into the binary; its [plugins] config section is empty."
          testId="deployment-plugins-empty"
        />
      )}

      {resolved.length > 0 && (
        <ul className="nt-plugins__list">
          {resolved.map((plugin) => (
            <li
              key={plugin.name}
              className="nt-plugins__row"
              data-testid={`deployment-plugin-${plugin.name}`}
            >
              <div className="nt-plugins__head">
                <code className="nt-plugins__name">{plugin.name}</code>
                <StatusPill tone={plugin.kind === 'remote' ? 'purple' : 'blue'}>
                  {plugin.kind === 'remote' ? 'remote process' : 'static (in-process)'}
                </StatusPill>
                {/* A remote plugin at full authority is the escalation
                    worth seeing at a glance, so the tones differ rather
                    than both reading neutral. */}
                <StatusPill tone={plugin.authority === 'full' ? 'magenta' : 'neutral'}>
                  {`authority: ${plugin.authority}`}
                </StatusPill>
                <StatusPill
                  tone={
                    plugin.risk === 'high' ? 'red' : plugin.risk === 'unknown' ? 'yellow' : 'green'
                  }
                >
                  {/* A static plugin's risk is the bound its tools
                      annotate, not the verdict a call receives — a tool
                      may override risk per call. Saying "risk" alone
                      would claim a prediction the server cannot make. */}
                  {plugin.kind === 'static'
                    ? `annotated risk: ${plugin.risk}`
                    : `risk: ${plugin.risk}`}
                </StatusPill>
              </div>
              {plugin.tools.length > 0 ? (
                <div className="nt-plugins__tools">
                  <span className="nt-plugins__label">tools</span>
                  {plugin.tools.map((tool) => (
                    <code key={tool} className="nt-plugins__tool">
                      {tool}
                    </code>
                  ))}
                </div>
              ) : (
                <div className="nt-plugins__tools">
                  <span className="nt-plugins__label">registers no tool</span>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

      {skipped.length > 0 && (
        <div className="nt-plugins__skipped" data-testid="deployment-plugins-skipped">
          <h3 className="nt-plugins__subtitle">{`Skipped (${skipped.length})`}</h3>
          <p className="nt-plugins__hint">
            Configured in this deployment&rsquo;s <code>[plugins]</code> section, but not installed
            — boot logged the reason and carried on, so the capability is missing.
          </p>
          <ul className="nt-plugins__list">
            {skipped.map((plugin, index) => (
              <li
                // The same name can be skipped twice (an entry and a
                // bind-time diagnostic), so the index is part of the key.
                key={`${plugin.name}-${index}`}
                className="nt-plugins__row nt-plugins__row--skipped"
                data-testid={`deployment-plugin-skipped-${index}`}
              >
                <code className="nt-plugins__name">{plugin.name}</code>
                <p className="nt-plugins__reason">{plugin.reason}</p>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Section>
  );
}
