---- MODULE SessionManagerFanout ----
EXTENDS FiniteSets

CONSTANTS
    Devices,
    FailBudgetInit,
    RemoveDiscoveryOnPartialExpansion

ASSUME Devices # {}
ASSUME FailBudgetInit \subseteq Devices

VARIABLES
    sent,
    discovery,
    knownDevices,
    msgQ,
    sessions,
    delivered,
    failBudget

vars == <<sent, discovery, knownDevices, msgQ, sessions, delivered, failBudget>>

Init ==
    /\ sent = FALSE
    /\ discovery = FALSE
    /\ knownDevices = {}
    /\ msgQ = {}
    /\ sessions = {}
    /\ delivered = {}
    /\ failBudget = FailBudgetInit

Send ==
    /\ ~sent
    /\ sent' = TRUE
    /\ IF knownDevices = {}
          THEN /\ discovery' = TRUE
               /\ msgQ' = msgQ
          ELSE /\ discovery' = discovery
               /\ msgQ' = msgQ \cup knownDevices
    /\ UNCHANGED <<knownDevices, sessions, delivered, failBudget>>

AppKeysUpdate ==
    /\ knownDevices # Devices
    /\ knownDevices' = Devices
    /\ UNCHANGED <<sent, discovery, msgQ, sessions, delivered, failBudget>>

ExpandDiscovery ==
    /\ discovery
    /\ knownDevices # {}
    /\ \E fail \in SUBSET (failBudget \cap knownDevices):
        LET succ == knownDevices \ fail IN
            /\ msgQ' = msgQ \cup succ
            /\ failBudget' = failBudget \ fail
            /\ discovery' =
                IF RemoveDiscoveryOnPartialExpansion
                    THEN FALSE
                    ELSE fail # {}
    /\ UNCHANGED <<sent, knownDevices, sessions, delivered>>

EstablishSession(d) ==
    /\ d \in knownDevices
    /\ d \notin sessions
    /\ sessions' = sessions \cup {d}
    /\ UNCHANGED <<sent, discovery, knownDevices, msgQ, delivered, failBudget>>

Flush(d) ==
    /\ d \in sessions
    /\ d \in msgQ
    /\ msgQ' = msgQ \ {d}
    /\ delivered' = delivered \cup {d}
    /\ UNCHANGED <<sent, discovery, knownDevices, sessions, failBudget>>

Stutter == UNCHANGED vars

Next ==
    \/ Send
    \/ AppKeysUpdate
    \/ ExpandDiscovery
    \/ \E d \in Devices: EstablishSession(d)
    \/ \E d \in Devices: Flush(d)
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(Send)
    /\ WF_vars(AppKeysUpdate)
    /\ WF_vars(ExpandDiscovery)
    /\ \A d \in Devices: WF_vars(EstablishSession(d))
    /\ \A d \in Devices: WF_vars(Flush(d))

\* Once we have sent, every known device must keep the message represented
\* somewhere: either still in discovery, queued per-device, or delivered.
NoDropKnown ==
    \A d \in knownDevices:
        ~sent \/ discovery \/ d \in msgQ \/ d \in delivered

\* Liveness goal under weak fairness:
\* every known device should eventually get delivered.
AllKnownEventuallyDelivered ==
    \A d \in Devices:
        []((sent /\ d \in knownDevices) => <>(d \in delivered))

====
