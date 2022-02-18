// Copyright 2021, The Tremor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// some tests don't use everything and this would generate warnings for those
// which it shouldn't
#![allow(dead_code)]

use async_std::{
    channel::{bounded, Receiver},
    prelude::FutureExt,
    task::JoinHandle,
};
use beef::Cow;
use halfbrown::HashMap;
use log::{debug, info};
use std::{sync::atomic::Ordering, time::Duration};
use tremor_common::{
    ids::ConnectorIdGen,
    url::ports::{ERR, IN, OUT},
};
use tremor_pipeline::{CbAction, EventId};
use tremor_runtime::{
    config,
    connectors::{
        self, builtin_connector_types, sink::SinkMsg, source::SourceMsg, Connectivity, StatusReport,
    },
    errors::Result,
    instance::InstanceState,
    pipeline::{self, CfMsg},
    system::{ShutdownMode, World, WorldConfig},
    Event, QSIZE,
};
use tremor_script::{ast::DeployEndpoint, Value};

pub(crate) struct ConnectorHarness {
    connector_id: String,
    world: World,
    handle: JoinHandle<Result<()>>,
    //config: config::Connector,
    addr: connectors::Addr,
    pipes: HashMap<Cow<'static, str>, TestPipeline>,
}

impl ConnectorHarness {
    pub(crate) async fn new_with_ports<T: ToString>(
        connector_type: T,
        defn: Value<'static>,
        ports: Vec<Cow<'static, str>>,
    ) -> Result<Self> {
        let mut connector_id_gen = ConnectorIdGen::new();
        let mut known_connectors = HashMap::new();

        for builder in builtin_connector_types() {
            known_connectors.insert(builder.connector_type(), builder);
        }

        let connector_type = connector_type.to_string();

        let (world, handle) = World::start(WorldConfig::default()).await?;
        let raw_config =
            config::Connector::from_config(connector_type.clone(), connector_type.into(), defn)?;
        let id = String::from("test");
        // FIXME: woohp whoop
        let connector_addr =
            connectors::spawn(&id, &mut connector_id_gen, &known_connectors, raw_config).await?;
        let mut pipes = HashMap::new();

        let (link_tx, link_rx) = async_std::channel::unbounded();
        for port in ports {
            // try to connect a fake pipeline outbound
            let pipeline_id = DeployEndpoint::new(format!("TEST__{}_pipeline", port), IN);
            let pipeline = TestPipeline::new(pipeline_id.alias().to_string());
            connector_addr
                .send(connectors::Msg::Link {
                    port: port.clone(),
                    pipelines: vec![(pipeline_id, pipeline.addr.clone())],
                    result_tx: link_tx.clone(),
                })
                .await?;

            if let Err(e) = link_rx.recv().await? {
                info!(
                    "Error connecting fake pipeline to port {} of connector {}: {}",
                    &port, id, e
                );
            } else {
                pipes.insert(port, pipeline);
            }
        }

        Ok(Self {
            connector_id: id,
            world,
            handle,
            //config: connector_config,
            addr: connector_addr,
            pipes,
        })
    }
    pub(crate) async fn new<T: ToString>(connector_type: T, defn: Value<'static>) -> Result<Self> {
        Self::new_with_ports(connector_type, defn, vec![IN, OUT, ERR]).await
    }

    pub(crate) async fn start(&self) -> Result<()> {
        // start the connector
        let (tx, rx) = bounded(1);
        self.addr.start(tx).await?;
        let cr = rx.recv().await?;
        cr.res?;

        // send a CBAction::open to the connector, so it starts pulling data
        self.addr
            .send_source(SourceMsg::Cb(CbAction::Open, EventId::default()))
            .await?;
        Ok(())
    }

    pub(crate) async fn pause(&self) -> Result<()> {
        Ok(self.addr.send(connectors::Msg::Pause).await?)
    }

    pub(crate) async fn resume(&self) -> Result<()> {
        Ok(self.addr.send(connectors::Msg::Resume).await?)
    }

    pub(crate) async fn stop(self) -> Result<(Vec<Event>, Vec<Event>)> {
        let (tx, rx) = bounded(1);

        self.addr.stop(tx).await?;
        let cr = rx.recv().await?;
        cr.res?;
        self.world.stop(ShutdownMode::Graceful).await?;
        //self.handle.cancel().await;
        let out_events = self
            .pipes
            .get(&OUT)
            .map(TestPipeline::get_events)
            .unwrap_or(Ok(vec![]))
            .unwrap_or_default();
        let err_events = self
            .pipes
            .get(&ERR)
            .map(TestPipeline::get_events)
            .unwrap_or(Ok(vec![]))
            .unwrap_or_default();
        Ok((out_events, err_events))
    }

    pub(crate) async fn status(&self) -> Result<StatusReport> {
        let (report_tx, report_rx) = bounded(1);
        self.addr.send(connectors::Msg::Report(report_tx)).await?;
        Ok(report_rx.recv().await?)
    }

    /// Wait for the connector to be connected.
    ///
    /// # Errors
    ///
    /// If communication with the connector fails or we time out without reaching connected state.
    pub(crate) async fn wait_for_connected(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        while self.status().await?.connectivity != Connectivity::Connected {
            // TODO create my own future here that succeeds on poll when status is connected
            async_std::task::sleep(Duration::from_millis(100)).await;
            if start.elapsed() >= timeout {
                return Err(format!(
                    "Connector {} didn't reach connected within {:?}",
                    self.connector_id, timeout
                )
                .into());
            }
        }
        Ok(())
    }

    /// Wait for the connecte to reach the given `state`.
    ///
    /// # Errors
    ///
    /// If communication with the connector fails or we time out without reaching the desired state
    pub(crate) async fn wait_for_state(
        &self,
        state: InstanceState,
        timeout: Duration,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        while self.status().await?.status != state {
            async_std::task::sleep(Duration::from_millis(100)).await;
            if start.elapsed() >= timeout {
                return Err(format!(
                    "Connector {} didn't reach state {} within {:?}",
                    self.connector_id, state, timeout
                )
                .into());
            }
        }
        Ok(())
    }

    pub(crate) fn get_pipe<T>(&self, port: T) -> Option<&TestPipeline>
    where
        T: Into<Cow<'static, str>>,
    {
        self.pipes.get(&port.into())
    }

    /// get the out pipeline - if any
    pub(crate) fn in_port(&self) -> Option<&TestPipeline> {
        self.get_pipe(IN)
    }

    /// get the out pipeline - if any
    pub(crate) fn out(&self) -> Option<&TestPipeline> {
        self.get_pipe(OUT)
    }

    /// get the err pipeline - if any
    pub(crate) fn err(&self) -> Option<&TestPipeline> {
        self.get_pipe(ERR)
    }

    pub(crate) async fn send_to_sink(&self, event: Event, port: Cow<'static, str>) -> Result<()> {
        self.addr.send_sink(SinkMsg::Event { event, port }).await
    }

    pub(crate) async fn send_contraflow(&self, cb: CbAction, id: EventId) -> Result<()> {
        self.addr.send_source(SourceMsg::Cb(cb, id)).await
    }
}

pub(crate) struct TestPipeline {
    rx: Receiver<Box<pipeline::Msg>>,
    rx_cf: Receiver<pipeline::CfMsg>,
    rx_mgmt: Receiver<pipeline::MgmtMsg>,
    addr: pipeline::Addr,
}

impl TestPipeline {
    pub(crate) fn new(alias: String) -> Self {
        let qsize = QSIZE.load(Ordering::Relaxed);
        let (tx, rx) = bounded(qsize);
        let (tx_cf, rx_cf) = bounded(qsize);
        let (tx_mgmt, rx_mgmt) = bounded(qsize);
        let addr = pipeline::Addr::new(tx, tx_cf, tx_mgmt, alias);
        Self {
            rx,
            rx_cf,
            rx_mgmt,
            addr,
        }
    }

    pub(crate) async fn send_contraflow(&self, event: Event) -> Result<()> {
        self.addr.send_insight(event).await
    }

    // get all available contraflow events
    pub(crate) fn get_contraflow_events(&self) -> Result<Vec<Event>> {
        let mut events = Vec::with_capacity(self.rx.len());
        while let Ok(CfMsg::Insight(event)) = self.rx_cf.try_recv() {
            events.push(event);
        }
        Ok(events)
    }

    // wait for a contraflow event
    pub(crate) async fn get_contraflow(&self) -> Result<Event> {
        match self.rx_cf.recv().await? {
            CfMsg::Insight(event) => Ok(event),
        }
    }

    // get all currently available events from the pipeline
    pub(crate) fn get_events(&self) -> Result<Vec<Event>> {
        let mut events = Vec::with_capacity(self.rx.len());
        while let Ok(msg) = self.rx.try_recv() {
            match *msg {
                pipeline::Msg::Event { event, .. } => {
                    events.push(event.clone());
                }
                pipeline::Msg::Signal(signal) => {
                    debug!("Received signal: {:?}", signal.kind)
                }
            }
        }
        Ok(events)
    }

    /// get a single event from the pipeline
    /// wait for up to 2 seconds for an event to arrive
    pub(crate) async fn get_event(&self) -> Result<Event> {
        loop {
            match self.rx.recv().timeout(Duration::from_secs(2)).await {
                Ok(Ok(msg)) => {
                    match *msg {
                        pipeline::Msg::Event { event, .. } => break Ok(event),
                        // filter out signals
                        pipeline::Msg::Signal(signal) => {
                            debug!("Received signal: {:?}", signal.kind)
                        }
                    }
                }
                Ok(Err(e)) => {
                    return Err(e.into());
                }
                Err(_) => {
                    return Err("Did not receive an event for 2 seconds".into());
                }
            }
        }
    }
}
