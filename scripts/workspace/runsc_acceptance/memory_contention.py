"""Evaluate existing native and durable evidence without performing I/O."""


def _uint(value, minimum=0):
    if type(value) is not int or not minimum <= value < 2**64:
        raise ValueError('bounded integer evidence required')
    return value


def _text(value):
    if not isinstance(value, str) or not value:
        raise ValueError('identity evidence required')
    return value


class _ObservedOom(ValueError):
    pass


def _limits(limits):
    if not isinstance(limits, dict):
        raise ValueError('recorded resource limits required')
    for scope in ('global', 'tenant'):
        for field in ('count', 'memoryBytes', 'pids', 'workspaceBytes'):
            _uint(limits[scope][field], 1)
    _uint(limits['hostHeadroomBytes'], 1)
    for field in ('memoryBytes', 'pids', 'workspaceBytes'):
        requested = _uint(limits['requested'][field], 1)
        if any(requested > limits[scope][field] for scope in ('global', 'tenant')):
            raise ValueError('single request must fit recorded resource limits')


def _bindings(runs, durable):
    if not isinstance(runs, list) or not runs or not isinstance(durable, dict):
        raise ValueError('original Run and durable Job evidence required')
    jobs, instances, environments, sessions = {}, {}, {}, {}
    for row in runs:
        run, session, environment, instance = (_text(row[field]) for field in
            ('agentRunId', 'sessionId', 'environmentId', 'instanceId'))
        job = 'agent_run.lifecycle:' + run
        if (job in jobs or instances.setdefault(instance, environment) != environment
                or environments.setdefault(environment, instance) != instance
                or sessions.setdefault(session, (environment, instance)) != (environment, instance)):
            raise ValueError('distinct original Run and environment bindings required')
        jobs[job] = row
    if len(instances) != len(environments) or len(sessions) != len(environments):
        raise ValueError('independent original environment bindings required')
    final = durable.get('finalJobs')
    if not isinstance(final, list) or len(final) != len(jobs):
        raise ValueError('exact final Job evidence required')
    seen = set()
    for row in final:
        job = row.get('jobId')
        binding = jobs.get(job)
        if (binding is None or job in seen or row.get('agentRunId') != binding['agentRunId']
                or row.get('sessionId') != binding['sessionId'] or row.get('jobKind') != 'agent_run.lifecycle'
                or row.get('status') != 'succeeded' or type(row.get('retryCount')) is not int
                or row['retryCount'] != 0 or type(row.get('maxRetries')) is not int
                or not 0 <= row['maxRetries'] < 2**32):
            raise ValueError('successful original Jobs without failed retries required')
        seen.add(job)
    return jobs, environments


def _snapshot(sample, environments, limits):
    """Invalid observations break continuity; an observed OOM rejects the gate."""
    try:
        if sample.get('schema') != 'workspace.perf.native_sample.v1' or sample.get('state') == 'unknown':
            return None
        window, host, states = sample['collectionWindow'], sample['host'], sample['states']
        start, end = (_uint(window[key]) for key in ('startedAtUnixMs', 'completedAtUnixMs'))
        if start > end or host['schema'] != 'native.host.resources.v1':
            return None
        host_id, boot = _text(host['hostId']), _text(host['bootId'])
        boot_at = _uint(host['observedAtBootMs'])
        duration = _uint(host['collectionDurationMs'])
        available, total = _uint(host['memAvailableBytes']), _uint(host['memTotalBytes'], 1)
        if duration > min(boot_at, 5000) or available > total:
            return None
        entries = host['containers']
        if not isinstance(entries, list) or not entries or len(entries) > 64:
            return None
        # A partial inventory cannot hide an OOM already observed elsewhere.
        for entry in entries:
            if entry.get('state') == 'observed':
                events = entry['resources']['memoryEvents']
                if any(type(events.get(field)) is int and events[field] > 0
                        for field in ('oom', 'oomKill', 'oomGroupKill')):
                    raise _ObservedOom('observed OOM disqualifies memory contention evidence')
        identities, container_ids, seen_environments = [], set(), set()
        memory = pids = workspace = 0
        for entry in entries:
            if entry.get('state') != 'observed':
                return None
            resources = entry['resources']
            events = resources['memoryEvents']
            if any(_uint(events[field]) for field in ('oom', 'oomKill')) or (
                    events.get('oomGroupKill') is not None and _uint(events['oomGroupKill'])):
                raise _ObservedOom('observed OOM disqualifies memory contention evidence')
            target = resources['target']
            environment, instance, container = (_text(target[field]) for field in
                ('environmentId', 'environmentInstanceId', 'containerId'))
            if (environments.get(environment) != instance or environment in seen_environments
                    or container in container_ids):
                return None
            state = states[container]
            if (state.get('running') is not True or type(state.get('paused')) is not bool
                    or state['caps']['memoryBytes'] != limits['requested']['memoryBytes']
                    or state['caps']['pids'] != limits['requested']['pids']):
                return None
            group = resources['cgroupIdentity']
            path = _text(group['path'])
            if not path.startswith('/sys/fs/cgroup/') or any(part in ('', '.', '..') for part in path.split('/')[1:]):
                return None
            identities.append((environment, instance, container, _text(target['profileDigest']),
                _text(target['imageDigest']), _uint(resources['initPid'], 1), _uint(resources['initStartTicks'], 1),
                path, _uint(group['device']), _uint(group['inode'], 1)))
            container_ids.add(container); seen_environments.add(environment)
            memory += _uint(resources['memoryCurrentBytes']) + _uint(resources['memorySwapCurrentBytes'])
            pids += _uint(resources['pidsCurrent'], 1)
            usage = resources['workspaceUsage']
            if usage.get('state') in ('observed', 'upperBound'):
                if usage['state'] == 'upperBound' and usage.get('source') != 'containerWritableLayer':
                    return None
                workspace += _uint(usage['bytes'])
            elif usage.get('state') == 'unknown':
                workspace += limits['requested']['workspaceBytes']
            else:
                return None
        if set(states) != container_ids:
            return None
        return {'start': start, 'end': end, 'bootAt': boot_at, 'host': host_id, 'boot': boot,
                'identity': tuple(sorted(identities)), 'environments': seen_environments,
                'memory': memory, 'pids': pids, 'workspace': workspace, 'available': available,
                'count': len(entries)}
    except _ObservedOom:
        raise
    except (KeyError, TypeError, ValueError, AttributeError):
        return None


def _consecutive(previous, current):
    return (previous is not None and previous['host'] == current['host']
            and previous['boot'] == current['boot'] and previous['identity'] == current['identity']
            and 0 <= current['start'] - previous['end'] <= 5000
            and 0 < current['bootAt'] - previous['bootAt'] <= 5000)


def _other_budgets_fit(sample, limits):
    requested = limits['requested']
    return (sample['memory'] + requested['memoryBytes'] <= limits['global']['memoryBytes']
            and sample['available'] >= limits['hostHeadroomBytes'] + requested['memoryBytes']
            and all(sample['count'] + 1 <= limits[scope]['count']
                    and sample['pids'] + requested['pids'] <= limits[scope]['pids']
                    and sample['workspace'] + requested['workspaceBytes'] <= limits[scope]['workspaceBytes']
                    for scope in ('global', 'tenant')))


def _proof(frames, waiting, claim, environment, limits, *, physical_cleanup=None):
    yielded, fallback = waiting['yieldedAtMs'], waiting['runAtMs']
    released = physical_cleanup['cleanupReceiptAtMs'] if physical_cleanup is not None else claim['releaseRecordedAtMs']
    claimed = claim['claimedAtMs']
    previous, streak, best = None, [], []
    for frame in frames:
        if (frame is None or not yielded <= frame['start'] <= frame['end'] <= released
                or environment in frame['environments'] or not _other_budgets_fit(frame, limits)
                or frame['memory'] + limits['requested']['memoryBytes'] <= limits['tenant']['memoryBytes']
                or (physical_cleanup is not None and (frame['host'] != claim['hostId']
                    or not any(identity[:3] == (physical_cleanup['environmentId'],physical_cleanup['instanceId'],physical_cleanup['containerId'])
                        for identity in frame['identity'])))):
            previous, streak = None, []
            continue
        streak = streak + [frame] if _consecutive(previous, frame) else [frame]
        if len(streak) > len(best):
            best = streak
        previous = frame
    if len(best) < 2:
        return None
    low = 'notObserved'
    previous = best[-1]
    for frame in frames[frames.index(best[-1]) + 1:]:
        if frame is None or not _consecutive(previous, frame):
            break
        previous = frame
        if (frame['start'] >= released and _other_budgets_fit(frame, limits)
                and frame['memory'] + limits['requested']['memoryBytes'] <= limits['tenant']['memoryBytes']):
            low = {'collectionStartedAtMs': frame['start'], 'collectionCompletedAtMs': frame['end'],
                   'measuredMemoryBytes': frame['memory']}
            break
    proof = {'jobId': waiting['jobId'], 'yieldedAtMs': yielded,
            'claimedAtMs': claimed, 'fallbackRunAtMs': fallback, 'pressureSampleCount': len(best),
            'firstCollectionStartedAtMs': best[0]['start'], 'lastCollectionCompletedAtMs': best[-1]['end'],
            'minimumMeasuredMemoryBytes': min(frame['memory'] for frame in best),
            'maximumMeasuredMemoryBytes': max(frame['memory'] for frame in best),
            'requestedMemoryBytes': limits['requested']['memoryBytes'],
            'tenantMemoryLimitBytes': limits['tenant']['memoryBytes'],
            'globalMemoryLimitBytes': limits['global']['memoryBytes'],
            'minimumHostAvailableBytes': min(frame['available'] for frame in best),
            'hostHeadroomBytes': limits['hostHeadroomBytes'],
            'maximumObservedCount': max(frame['count'] for frame in best),
            'maximumObservedPids': max(frame['pids'] for frame in best),
            'maximumWorkspaceChargeBytes': max(frame['workspace'] for frame in best)}
    if physical_cleanup is None:
        proof.update(releaseRecordedAtMs=released,postReleaseMemory=low)
    else:
        # The removed holder is physical evidence; the direct retry hint remains
        # host preparation. Neither timestamp is substituted for the other.
        proof.update(wakeSource='hostPreparation',hostId=claim['hostId'],generation=claim['generation'],
            availabilityScope=claim['availabilityScope'],hostPreparationAvailableAtMs=claim['hostPreparationAvailableAtMs'],
            hostPreparationToClaimMs=claim['hostPreparationToClaimMs'],
            physicalCleanupToAvailabilityMs=claim['hostPreparationAvailableAtMs']-released,
            physicalCleanup={key:physical_cleanup[key] for key in ('agentRunId','sessionId','environmentId','instanceId',
                'containerId','cleanupReceiptAtMs','cleanupComplete','physicalRemoved','containerAbsent',
                'residentHoldingCount','historyPreserved')})
    return proof


def _per_run_cleanup(proofs, runs):
    # Reuse the independent original-allocation, physical absence and history
    # contract before any proof can explain a pressure holder's release.
    from runsc_acceptance.per_run import validate_cleanup
    import re
    validate_cleanup(proofs, runs)
    for proof in proofs:
        _uint(proof['cleanupReceiptAtMs'])
        if not re.fullmatch('[a-f0-9]{64}',proof['containerId']):
            raise ValueError('exact original cleanup container identity required')
    return proofs


def _episode_claim(durable, waiting):
    """A later retry cannot borrow pressure from an already consumed wait."""
    job, yielded = waiting['jobId'], _uint(waiting['yieldedAtMs'])
    claims = [claim for claim in durable['capacityClaims'] if claim.get('jobId') == job
              and _uint(claim['claimedAtMs']) >= yielded]
    if not claims:
        return None
    claim = min(claims, key=lambda row: _uint(row['claimedAtMs']))
    next_yield = min((_uint(row['yieldedAtMs']) for row in durable['jobYields']
                      if row.get('jobId') == job and _uint(row['yieldedAtMs']) > yielded), default=2**64)
    return claim if _uint(claim['claimedAtMs']) < next_yield else None


def evaluate(samples, durable, runs, limits, *, per_run_cleanup=None):
    """Require tenant pressure, physical release evidence and the original Job's retry.

    The direct release-wake path remains independent. Per-Run cleanup can also
    prove removal before a host-preparation retry hint without relabeling it.
    A later low-water observation is optional and only reported with identical
    holder/cgroup identities. Completion proves recovery, never released bytes.
    """
    try:
        _limits(limits)
        jobs, environments = _bindings(runs, durable)
        cleanup = _per_run_cleanup(per_run_cleanup, runs) if per_run_cleanup is not None else []
        if not isinstance(samples, list):
            raise ValueError('native sample sequence required')
        frames = [_snapshot(sample, environments, limits) for sample in samples]
        if not isinstance(durable.get('jobYields'), list) or not isinstance(durable.get('capacityClaims'), list):
            raise ValueError('durable yield and capacity claim sequence required')
        proofs, preparation_proofs = [], {}
        matching_yields = 0
        for waiting in durable['jobYields']:
            if waiting.get('transitionReason') != 'execution_resident_capacity_wait':
                continue
            job = waiting.get('jobId'); binding = jobs.get(job)
            if (binding is None or waiting.get('agentRunId') != binding['agentRunId']
                    or waiting.get('sessionId') != binding['sessionId']):
                raise ValueError('original waiting Job binding required')
            yielded, fallback = _uint(waiting['yieldedAtMs']), _uint(waiting['runAtMs'])
            claim = _episode_claim(durable, waiting)
            if claim is None:
                continue
            if claim.get('agentRunId') != binding['agentRunId']:
                raise ValueError('original release-claimed Job binding required')
            proof = None
            if claim.get('wakeSource') == 'physicalRelease':
                released, claimed = _uint(claim['releaseRecordedAtMs']), _uint(claim['claimedAtMs'])
                if yielded <= released <= claimed < fallback:
                    proof = _proof(frames, waiting, claim, binding['environmentId'], limits)
            elif claim.get('wakeSource') == 'hostPreparation' and cleanup:
                available, claimed = _uint(claim['hostPreparationAvailableAtMs']), _uint(claim['claimedAtMs'])
                _uint(claim['generation'],1)
                _text(claim['hostId'])
                if (claim.get('availabilityScope') != 'globalPreparation'
                        or _uint(claim['hostPreparationToClaimMs']) != claimed-available):
                    raise ValueError('exact host preparation availability evidence required')
                for holder in cleanup:
                    if not yielded <= holder['cleanupReceiptAtMs'] <= available <= claimed < fallback:
                        continue
                    proof = _proof(frames, waiting, claim, binding['environmentId'], limits,physical_cleanup=holder)
                    if proof is not None:
                        break
            if proof is not None:
                matching_yields += 1
                if proof.get('wakeSource') == 'hostPreparation':
                    holder = proof['physicalCleanup']
                    key = (job,holder['instanceId'],holder['cleanupReceiptAtMs'],proof['generation'],proof['claimedAtMs'])
                    previous = preparation_proofs.get(key)
                    strength = lambda item:(item['pressureSampleCount'],
                        item['lastCollectionCompletedAtMs']-item['firstCollectionStartedAtMs'],-item['yieldedAtMs'])
                    if previous is None or strength(proof)>strength(previous):
                        preparation_proofs[key] = proof
                else:
                    proofs.append(proof)
        proofs.extend(preparation_proofs.values())
        if not proofs:
            raise ValueError('two consecutive measured tenant-pressure samples, physical release evidence and original retry before fallback required')
        return {'schema': 'runsc.shared.memory_contention.v1', 'status': 'verified', 'pressureProofs': proofs,
                'matchingYieldProofs':matching_yields,'uniqueWaitingJobs':len({proof['jobId'] for proof in proofs}),
                'uniqueWakeClaims':len({(proof['jobId'],proof.get('generation'),proof['claimedAtMs']) for proof in proofs})}
    except _ObservedOom:
        raise
    except (KeyError, TypeError, AttributeError):
        raise ValueError('complete memory contention evidence required') from None
