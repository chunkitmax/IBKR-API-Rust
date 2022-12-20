//! Receives messages from Reader, decodes messages, and feeds them to Wrapper
use std::{collections::HashSet, thread, sync::atomic::{AtomicBool, Ordering}, rc::Rc};

use std::marker::Sync;
use std::ops::Deref;
use std::slice::Iter;
use std::str::FromStr;
use std::string::ToString;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use bigdecimal::BigDecimal;
use float_cmp::*;
use log::*;
use num_traits::float::FloatCore;
use num_traits::FromPrimitive;

use crate::core::client::{ConnStatus, POISONED_MUTEX};
use crate::core::common::{
    BarData, CommissionReport, DepthMktDataDescription, FamilyCode, HistogramData, HistoricalTick,
    HistoricalTickBidAsk, HistoricalTickLast, NewsProvider, PriceIncrement, RealTimeBar,
    SmartComponent, TagValue, TickAttrib, TickAttribBidAsk, TickAttribLast, TickType, MAX_MSG_LEN,
    NO_VALID_ID, UNSET_DOUBLE, UNSET_INTEGER,
};
use crate::core::contract::{Contract, ContractDescription, ContractDetails, DeltaNeutralContract};
use crate::core::errors::{IBKRApiLibError, TwsError};
use crate::core::execution::Execution;
use crate::core::messages::{read_fields, IncomingMessageIds};
use crate::core::order::{Order, OrderState, SoftDollarTier};
use crate::core::order_decoder::OrderDecoder;
use crate::core::scanner::ScanData;
use crate::core::server_versions::{
    MIN_SERVER_VER_AGG_GROUP, MIN_SERVER_VER_FRACTIONAL_POSITIONS, MIN_SERVER_VER_LAST_LIQUIDITY,
    MIN_SERVER_VER_MARKET_CAP_PRICE, MIN_SERVER_VER_MARKET_RULES,
    MIN_SERVER_VER_MD_SIZE_MULTIPLIER, MIN_SERVER_VER_MODELS_SUPPORT,
    MIN_SERVER_VER_ORDER_CONTAINER, MIN_SERVER_VER_PAST_LIMIT, MIN_SERVER_VER_PRE_OPEN_BID_ASK,
    MIN_SERVER_VER_REALIZED_PNL, MIN_SERVER_VER_REAL_EXPIRATION_DATE,
    MIN_SERVER_VER_SERVICE_DATA_TYPE, MIN_SERVER_VER_SMART_DEPTH,
    MIN_SERVER_VER_SYNT_REALTIME_BARS, MIN_SERVER_VER_UNDERLYING_INFO,
    MIN_SERVER_VER_UNREALIZED_PNL,
};
use crate::core::wrapper::Wrapper;

const WRAPPER_POISONED_MUTEX: &str = "Wrapper mutex was poisoned";
//==================================================================================================
pub fn decode_i32(iter: &mut Iter<String>) -> Result<i32, IBKRApiLibError> {
    let next = iter.next();

    let val: i32 = next.unwrap().parse().unwrap_or(0);
    Ok(val)
}

//==================================================================================================
pub fn decode_i32_show_unset(iter: &mut Iter<String>) -> Result<i32, IBKRApiLibError> {
    let next = iter.next();
    //info!("{:?}", next);
    let retval: i32 = next.unwrap().parse().unwrap_or(0);
    Ok(if retval == 0 { UNSET_INTEGER } else { retval })
}

//==================================================================================================
pub fn decode_i64(iter: &mut Iter<String>) -> Result<i64, IBKRApiLibError> {
    let next = iter.next();
    //info!("{:?}", next);
    let val: i64 = next.unwrap().parse().unwrap_or(0);
    Ok(val)
}

//==================================================================================================
pub fn decode_f64(iter: &mut Iter<String>) -> Result<f64, IBKRApiLibError> {
    let next = iter.next();
    //info!("{:?}", next);
    let val = next.unwrap().parse().unwrap_or(0.0);
    Ok(val)
}

//==================================================================================================
pub fn decode_f64_show_unset(iter: &mut Iter<String>) -> Result<f64, IBKRApiLibError> {
    let next = iter.next();
    //info!("{:?}", next);
    let retval: f64 = next.unwrap().parse().unwrap_or(0.0);
    Ok(if retval == 0.0 { UNSET_DOUBLE } else { retval })
}

//==================================================================================================
pub fn decode_string(iter: &mut Iter<String>) -> Result<String, IBKRApiLibError> {
    let next = iter.next();
    //info!("{:?}", next);
    let val = next.unwrap().parse().unwrap_or("".to_string());
    Ok(val)
}

//==================================================================================================
pub fn decode_bool(iter: &mut Iter<String>) -> Result<bool, IBKRApiLibError> {
    let next = iter.next();
    //info!("{:?}", next);
    let retval: i32 = next.unwrap_or(&"0".to_string()).parse().unwrap_or(0);
    Ok(retval != 0)
}

//==================================================================================================
pub struct Decoder<T: Wrapper> {
    msg_queue: Receiver<String>,
    pub wrapper: Arc<T>,
    pub server_version: i32,
    conn_state: Arc<Mutex<ConnStatus>>,
}

impl<T> Decoder<T>
where
    T: Wrapper + Sync + 'static,
{
    pub fn new(
        the_wrapper: Arc<T>,
        msg_queue: Receiver<String>,
        server_version: i32,
        conn_state: Arc<Mutex<ConnStatus>>,
    ) -> Self {
        Decoder {
            wrapper: the_wrapper,
            msg_queue: msg_queue,
            server_version,
            conn_state,
        }
    }

    //----------------------------------------------------------------------------------------------
    pub fn run(&self) -> bool {
        //This is the function that has the message loop.
        const CONN_STATE_POISONED: &str = "Connection state mutex was poisoned";
        info!("Starting run...");

        let server_version = self.server_version;
        let wrapper = self.wrapper.clone();

        let is_running = Arc::new(AtomicBool::new(true));
        let err = Arc::new(AtomicBool::new(false));

        let is_running_loop = is_running.clone();
        while is_running_loop.load(Ordering::SeqCst) {
            // debug!("Client waiting for message...");
            let is_running_iter = is_running.clone();
            let wrapper = wrapper.clone();
            let err_iter = err.clone();
            match self.msg_queue.recv() {
              Result::Ok(val) => {
                // let _self = _self.clone();
                // let conn_state = self.conn_state.clone();
                thread::spawn(move || {
                    if val.len() > MAX_MSG_LEN as usize {
                        wrapper.error(
                            NO_VALID_ID,
                            TwsError::NotConnected.code(),
                            format!("{}:{}:{}", TwsError::NotConnected.message(), val.len(), val)
                                .as_str(),
                        );
                        error!("Error receiving message.  Disconnected: Message too big");
                        wrapper
                            .connection_closed();
                        // *conn_state.lock().expect(CONN_STATE_POISONED) =
                        //     ConnStatus::DISCONNECTED;
                        error!("Error receiving message.  Invalid size.  Disconnected.");
                        is_running_iter.store(false, Ordering::SeqCst);
                    } else {
                        let fields = read_fields((&val).as_ref());

                        if let Err(e) = Self::interpret(wrapper, server_version, fields.as_slice()) {
                          err_iter.store(true, Ordering::SeqCst);
                          is_running_iter.store(false, Ordering::SeqCst);
                        }
                    }
                });
              }
              Result::Err(err) => {
                  if *self.conn_state.lock().expect(CONN_STATE_POISONED).deref() as i32
                      != ConnStatus::DISCONNECTED as i32
                  {
                      info!("Error receiving message.  Disconnected: {:?}", err);
                      self.wrapper.clone()
                          .connection_closed();
                      *self.conn_state.lock().expect(CONN_STATE_POISONED) =
                          ConnStatus::DISCONNECTED;

                      is_running_iter.store(false, Ordering::SeqCst);
                  } else {
                      error!("Disconnected...");
                      is_running_iter.store(false, Ordering::SeqCst);
                  }
              }
            };
        }
        !err.load(Ordering::SeqCst)
    }
}


impl<T> Decoder<T>
where
    T: Wrapper + Sync,
{

  //----------------------------------------------------------------------------------------------
  pub fn interpret(wrapper: Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      if fields.is_empty() {
          return Ok(());
      }

      let msg_id = i32::from_str(fields.get(0).unwrap().as_str())?;

      match FromPrimitive::from_i32(msg_id) {
          Some(IncomingMessageIds::TickPrice) => Self::process_tick_price(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::AccountSummary) => Self::process_account_summary(&wrapper, fields)?,
          Some(IncomingMessageIds::AccountSummaryEnd) => {
              Self::process_account_summary_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::AccountUpdateMulti) => {
              Self::process_account_update_multi(&wrapper, fields)?
          }
          Some(IncomingMessageIds::AccountUpdateMultiEnd) => {
              Self::process_account_update_multi_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::AcctDownloadEnd) => {
              Self::process_account_download_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::AcctUpdateTime) => Self::process_account_update_time(&wrapper, fields)?,
          Some(IncomingMessageIds::AcctValue) => Self::process_account_value(&wrapper, fields)?,
          Some(IncomingMessageIds::BondContractData) => {
              Self::process_bond_contract_data(&wrapper, server_version, fields)?
          }
          Some(IncomingMessageIds::CommissionReport) => Self::process_commission_report(&wrapper, fields)?,
          Some(IncomingMessageIds::CompletedOrder) => Self::process_completed_order(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::CompletedOrdersEnd) => {
              Self::process_complete_orders_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::ContractData) => Self::process_contract_details(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::ContractDataEnd) => {
              Self::process_contract_details_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::CurrentTime) => Self::process_current_time(&wrapper, fields)?,
          Some(IncomingMessageIds::DeltaNeutralValidation) => {
              Self::process_delta_neutral_validation(&wrapper, fields)?
          }
          Some(IncomingMessageIds::DisplayGroupList) => {
              Self::process_display_group_list(&wrapper, fields)?
          }
          Some(IncomingMessageIds::DisplayGroupUpdated) => {
              Self::process_display_group_updated(&wrapper, fields)?
          }
          Some(IncomingMessageIds::ErrMsg) => Self::process_error_message(&wrapper, fields)?,
          Some(IncomingMessageIds::ExecutionData) => Self::process_execution_data(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::ExecutionDataEnd) => {
              Self::process_execution_data_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::FamilyCodes) => Self::process_family_codes(&wrapper, fields)?,
          Some(IncomingMessageIds::FundamentalData) => Self::process_fundamental_data(&wrapper, fields)?,
          Some(IncomingMessageIds::HeadTimestamp) => Self::process_head_timestamp(&wrapper, fields)?,
          Some(IncomingMessageIds::HistogramData) => Self::process_histogram_data(&wrapper, fields)?,
          Some(IncomingMessageIds::HistoricalData) => Self::process_historical_data(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::HistoricalDataUpdate) => {
              Self::process_historical_data_update(&wrapper, fields)?
          }
          Some(IncomingMessageIds::HistoricalNews) => Self::process_historical_news(&wrapper, fields)?,
          Some(IncomingMessageIds::HistoricalNewsEnd) => {
              Self::process_historical_news_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::HistoricalTicks) => Self::process_historical_ticks(&wrapper, fields)?,
          Some(IncomingMessageIds::HistoricalTicksBidAsk) => {
              Self::process_historical_ticks_bid_ask(&wrapper, fields)?
          }

          Some(IncomingMessageIds::HistoricalTicksLast) => {
              Self::process_historical_ticks_last(&wrapper, fields)?
          }
          Some(IncomingMessageIds::ManagedAccts) => Self::process_managed_accounts(&wrapper, fields)?,
          Some(IncomingMessageIds::MarketDataType) => Self::process_market_data_type(&wrapper, fields)?,
          Some(IncomingMessageIds::MarketDepth) => Self::process_market_depth(&wrapper, fields)?,
          Some(IncomingMessageIds::MarketDepthL2) => Self::process_market_depth_l2(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::MarketRule) => Self::process_market_rule(&wrapper, fields)?,
          Some(IncomingMessageIds::MktDepthExchanges) => {
              Self::process_market_depth_exchanges(&wrapper, server_version, fields)?
          }
          Some(IncomingMessageIds::NewsArticle) => Self::process_news_article(&wrapper, fields)?,
          Some(IncomingMessageIds::NewsBulletins) => Self::process_news_bulletins(&wrapper, fields)?,
          Some(IncomingMessageIds::NewsProviders) => Self::process_news_providers(&wrapper, fields)?,
          Some(IncomingMessageIds::NextValidId) => Self::process_next_valid_id(&wrapper, fields)?,
          Some(IncomingMessageIds::OpenOrder) => Self::process_open_order(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::OpenOrderEnd) => Self::process_open_order_end(&wrapper, fields)?,
          Some(IncomingMessageIds::OrderStatus) => Self::process_order_status(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::OrderBound) => Self::process_order_bound(&wrapper, fields)?,
          Some(IncomingMessageIds::Pnl) => Self::process_pnl(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::PnlSingle) => Self::process_pnl_single(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::PortfolioValue) => Self::process_portfolio_value(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::PositionData) => Self::process_position_data(&wrapper, server_version, fields)?,
          Some(IncomingMessageIds::PositionEnd) => Self::process_position_end(&wrapper, fields)?,
          Some(IncomingMessageIds::RealTimeBars) => Self::process_real_time_bars(&wrapper, fields)?,
          Some(IncomingMessageIds::ReceiveFa) => Self::process_receive_fa(&wrapper, fields)?,
          Some(IncomingMessageIds::RerouteMktDataReq) => {
              Self::process_reroute_mkt_data_req(&wrapper, fields)?
          }

          Some(IncomingMessageIds::PositionMulti) => Self::process_position_multi(&wrapper, fields)?,
          Some(IncomingMessageIds::PositionMultiEnd) => {
              Self::process_position_multi_end(&wrapper, fields)?
          }
          Some(IncomingMessageIds::ScannerData) => Self::process_scanner_data(&wrapper, fields)?,
          Some(IncomingMessageIds::ScannerParameters) => {
              Self::process_scanner_parameters(&wrapper, fields)?
          }
          Some(IncomingMessageIds::SecurityDefinitionOptionParameter) => {
              Self::process_security_definition_option_parameter(&wrapper, fields)?
          }
          Some(IncomingMessageIds::SecurityDefinitionOptionParameterEnd) => {
              Self::process_security_definition_option_parameter_end(&wrapper, fields)?
          }

          Some(IncomingMessageIds::SmartComponents) => Self::process_smart_components(&wrapper, fields)?,
          Some(IncomingMessageIds::SoftDollarTiers) => Self::process_soft_dollar_tiers(&wrapper, fields)?,
          Some(IncomingMessageIds::SymbolSamples) => Self::process_symbol_samples(&wrapper, fields)?,
          Some(IncomingMessageIds::TickByTick) => Self::process_tick_by_tick(&wrapper, fields)?,
          Some(IncomingMessageIds::TickEfp) => Self::process_tick_by_tick(&wrapper, fields)?,
          Some(IncomingMessageIds::TickGeneric) => Self::process_tick_generic(&wrapper, fields)?,
          Some(IncomingMessageIds::TickNews) => Self::process_tick_news(&wrapper, fields)?,
          Some(IncomingMessageIds::TickOptionComputation) => {
              Self::process_tick_option_computation(&wrapper, fields)?
          }
          Some(IncomingMessageIds::TickReqParams) => Self::process_tick_req_params(&wrapper, fields)?,
          Some(IncomingMessageIds::TickSize) => Self::process_tick_size(&wrapper, fields)?,
          Some(IncomingMessageIds::TickSnapshotEnd) => Self::process_tick_snapshot_end(&wrapper, fields)?,
          Some(IncomingMessageIds::TickString) => Self::process_tick_string(&wrapper, fields)?,
          Some(IncomingMessageIds::VerifyAndAuthCompleted) => {
              Self::process_verify_and_auth_completed(&wrapper, fields)?
          }

          Some(IncomingMessageIds::VerifyCompleted) => Self::process_verify_completed(&wrapper, fields)?,

          Some(IncomingMessageIds::VerifyMessageApi) => Self::process_verify_completed(&wrapper, fields)?,

          Some(IncomingMessageIds::VerifyAndAuthMessageApi) => {
              Self::process_verify_and_auth_message_api(&wrapper, fields)?
          }
          Some(IncomingMessageIds::RerouteMktDepthReq) => {
              Self::process_reroute_mkt_depth_req(&wrapper, fields)?
          }

          _ => panic!("Received unkown message id!!  Exiting..."),
      }
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_price(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let tick_type: i32 = decode_i32(&mut fields_itr)?;
      let price: f64 = decode_f64(&mut fields_itr)?;
      let size = decode_i32(&mut fields_itr)?;
      let attr_mask: i32 = decode_i32(&mut fields_itr)?;
      let mut tick_arrtibute = TickAttrib::new(false, false, false);

      tick_arrtibute.can_auto_execute = attr_mask == 1;

      if server_version >= MIN_SERVER_VER_PAST_LIMIT {
          tick_arrtibute.can_auto_execute = attr_mask & 1 != 0;
          tick_arrtibute.past_limit = attr_mask & 2 != 0;
      }
      if server_version >= MIN_SERVER_VER_PRE_OPEN_BID_ASK {
          tick_arrtibute.pre_open = attr_mask & 4 != 0;
      }
      wrapper.clone()
          .tick_price(
              req_id,
              FromPrimitive::from_i32(tick_type).unwrap(),
              price,
              tick_arrtibute,
          );

      // process ver 2 fields

      let size_tick_type = match FromPrimitive::from_i32(tick_type) {
          Some(TickType::Bid) => TickType::BidSize,
          Some(TickType::Ask) => TickType::AskSize,
          Some(TickType::Last) => TickType::LastSize,
          Some(TickType::DelayedBid) => TickType::DelayedBidSize,
          Some(TickType::DelayedAsk) => TickType::DelayedAskSize,
          Some(TickType::DelayedLast) => TickType::DelayedLastSize,
          _ => TickType::NotSet,
      };

      if size_tick_type as i32 != TickType::NotSet as i32 {
          wrapper.clone()
              .tick_size(req_id, size_tick_type, size);
      }
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_string(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id: i32 = decode_i32(&mut fields_itr)?;
      let tick_type: i32 = decode_i32(&mut fields_itr)?;
      let value = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .tick_string(
              req_id,
              FromPrimitive::from_i32(tick_type).unwrap(),
              value.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_summary(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone()
          .account_summary(
              decode_i32(&mut fields_itr)?,
              decode_string(&mut fields_itr)?.as_ref(),
              decode_string(&mut fields_itr)?.as_ref(),
              decode_string(&mut fields_itr)?.as_ref(),
              decode_string(&mut fields_itr)?.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_summary_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone()
          .account_summary_end(decode_i32(&mut fields_itr)?);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_update_multi(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id: i32 = decode_i32(&mut fields_itr)?;
      let account = decode_string(&mut fields_itr)?;
      let model_code = decode_string(&mut fields_itr)?;
      let key = decode_string(&mut fields_itr)?;
      let value = decode_string(&mut fields_itr)?;
      let currency = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .account_update_multi(
              req_id,
              account.as_ref(),
              model_code.as_ref(),
              key.as_ref(),
              value.as_ref(),
              currency.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_update_multi_end(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id: i32 = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .account_update_multi_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_download_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone()
          .account_download_end(decode_string(&mut fields_itr)?.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_update_time(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone()
          .update_account_time(decode_string(&mut fields_itr)?.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_account_value(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone()
          .update_account_value(
              decode_string(&mut fields_itr)?.as_ref(),
              decode_string(&mut fields_itr)?.as_ref(),
              decode_string(&mut fields_itr)?.as_ref(),
              decode_string(&mut fields_itr)?.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_bond_contract_data(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let version: i32 = decode_i32(&mut fields_itr)?;

      let mut req_id = -1;
      if version >= 3 {
          req_id = decode_i32(&mut fields_itr)?;
      }

      let mut contract = ContractDetails::default();

      contract.contract.symbol = decode_string(&mut fields_itr)?;
      contract.contract.sec_type = decode_string(&mut fields_itr)?;
      contract.cusip = decode_string(&mut fields_itr)?;
      contract.coupon = decode_f64(&mut fields_itr)?;
      Self::read_last_trade_date(wrapper, &mut contract, true, fields_itr.next().unwrap())?;
      contract.issue_date = decode_string(&mut fields_itr)?;
      contract.ratings = decode_string(&mut fields_itr)?;
      contract.bond_type = decode_string(&mut fields_itr)?;
      contract.coupon_type = decode_string(&mut fields_itr)?;
      contract.convertible = i32::from_str(fields_itr.next().unwrap().as_ref())? != 0;
      contract.callable = i32::from_str(fields_itr.next().unwrap().as_ref())? != 0;
      contract.putable = i32::from_str(fields_itr.next().unwrap().as_ref())? != 0;
      contract.desc_append = decode_string(&mut fields_itr)?;
      contract.contract.exchange = decode_string(&mut fields_itr)?;
      contract.contract.currency = decode_string(&mut fields_itr)?;
      contract.market_name = decode_string(&mut fields_itr)?;
      contract.contract.trading_class = decode_string(&mut fields_itr)?;
      contract.contract.con_id = decode_i32(&mut fields_itr)?;
      contract.min_tick = decode_f64(&mut fields_itr)?;
      if server_version >= MIN_SERVER_VER_MD_SIZE_MULTIPLIER {
          contract.md_size_multiplier = decode_i32(&mut fields_itr)?;
      }
      contract.order_types = decode_string(&mut fields_itr)?;
      contract.valid_exchanges = decode_string(&mut fields_itr)?;
      if version >= 2 {
          contract.next_option_date = decode_string(&mut fields_itr)?;
          contract.next_option_type = decode_string(&mut fields_itr)?;
          contract.next_option_partial = decode_bool(&mut fields_itr)?;
          contract.notes = decode_string(&mut fields_itr)?;
      }
      if version >= 4 {
          contract.long_name = decode_string(&mut fields_itr)?;
      }
      if version >= 6 {
          contract.ev_rule = decode_string(&mut fields_itr)?;
          contract.ev_multiplier = decode_f64(&mut fields_itr)?;
      }
      if version >= 5 {
          let sec_id_list_count = decode_i32(&mut fields_itr)?;
          if sec_id_list_count > 0 {
              contract.sec_id_list = vec![];
              for _ in 0..sec_id_list_count {
                  contract.sec_id_list.push(TagValue::new(
                      fields_itr.next().unwrap().parse().unwrap(),
                      fields_itr.next().unwrap().parse().unwrap(),
                  ));
              }
          }
      }
      if server_version >= MIN_SERVER_VER_AGG_GROUP {
          contract.agg_group = decode_i32(&mut fields_itr)?;
      }
      if server_version >= MIN_SERVER_VER_MARKET_RULES {
          contract.market_rule_ids = decode_string(&mut fields_itr)?;
      }

      wrapper.clone()
          .bond_contract_details(req_id, contract.clone());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_commission_report(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let mut commission_report = CommissionReport::default();
      commission_report.exec_id = fields_itr.next().unwrap().to_string();
      commission_report.commission = decode_f64(&mut fields_itr)?;
      commission_report.currency = fields_itr.next().unwrap().to_string();

      commission_report.realized_pnl = decode_f64(&mut fields_itr)?;

      commission_report.yield_ = decode_f64(&mut fields_itr)?;

      commission_report.yield_redemption_date = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .commission_report(commission_report);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_completed_order(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let mut contract = Contract::default();
      let mut order = Order::default();
      let mut order_state = OrderState::default();

      let mut order_decoder = OrderDecoder::new(
          &mut contract,
          &mut order,
          &mut order_state,
          UNSET_INTEGER,
          server_version,
      );

      order_decoder.decode_completed(&mut fields_itr)?;

      wrapper.clone()
          .completed_order(contract, order, order_state);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_complete_orders_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      wrapper.clone()
          .completed_orders_end();
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_contract_details(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let version: i32 = decode_i32(&mut fields_itr)?;

      let mut req_id = -1;
      if version >= 3 {
          req_id = decode_i32(&mut fields_itr)?;
      }

      let mut contract = ContractDetails::default();

      contract.contract.symbol = decode_string(&mut fields_itr)?;
      contract.contract.sec_type = decode_string(&mut fields_itr)?;
      Self::read_last_trade_date(wrapper, &mut contract, false, fields_itr.next().unwrap())?;
      contract.contract.strike = decode_f64(&mut fields_itr)?;
      contract.contract.right = decode_string(&mut fields_itr)?;
      contract.contract.exchange = decode_string(&mut fields_itr)?;
      contract.contract.currency = decode_string(&mut fields_itr)?;
      contract.contract.local_symbol = decode_string(&mut fields_itr)?;
      contract.market_name = decode_string(&mut fields_itr)?;
      contract.contract.trading_class = decode_string(&mut fields_itr)?;
      contract.contract.con_id = decode_i32(&mut fields_itr)?;
      contract.min_tick = decode_f64(&mut fields_itr)?;
      if server_version >= MIN_SERVER_VER_MD_SIZE_MULTIPLIER {
          contract.md_size_multiplier = decode_i32(&mut fields_itr)?;
      }
      contract.contract.multiplier = decode_string(&mut fields_itr)?;
      contract.order_types = decode_string(&mut fields_itr)?;
      contract.valid_exchanges = decode_string(&mut fields_itr)?;
      contract.price_magnifier = decode_i32(&mut fields_itr)?;
      if version >= 4 {
          contract.under_con_id = decode_i32(&mut fields_itr)?;
      }
      if version >= 5 {
          contract.long_name = decode_string(&mut fields_itr)?;
          contract.contract.primary_exchange = decode_string(&mut fields_itr)?;
      }

      if version >= 6 {
          contract.contract_month = decode_string(&mut fields_itr)?;
          contract.industry = decode_string(&mut fields_itr)?;
          contract.category = decode_string(&mut fields_itr)?;
          contract.subcategory = decode_string(&mut fields_itr)?;
          contract.time_zone_id = decode_string(&mut fields_itr)?;
          contract.trading_hours = decode_string(&mut fields_itr)?;
          contract.liquid_hours = decode_string(&mut fields_itr)?;
      }
      if version >= 8 {
          contract.ev_rule = decode_string(&mut fields_itr)?;
          contract.ev_multiplier = decode_f64(&mut fields_itr)?;
      }

      if version >= 7 {
          let sec_id_list_count = decode_i32(&mut fields_itr)?;
          if sec_id_list_count > 0 {
              contract.sec_id_list = vec![];
              for _ in 0..sec_id_list_count {
                  contract.sec_id_list.push(TagValue::new(
                      decode_string(&mut fields_itr)?,
                      decode_string(&mut fields_itr)?,
                  ));
              }
          }
      }
      if server_version >= MIN_SERVER_VER_AGG_GROUP {
          contract.agg_group = decode_i32(&mut fields_itr)?;
      }

      if server_version >= MIN_SERVER_VER_UNDERLYING_INFO {
          contract.under_symbol = decode_string(&mut fields_itr)?;
          contract.under_sec_type = decode_string(&mut fields_itr)?;
      }
      if server_version >= MIN_SERVER_VER_MARKET_RULES {
          contract.market_rule_ids = decode_string(&mut fields_itr)?;
      }

      if server_version >= MIN_SERVER_VER_REAL_EXPIRATION_DATE {
          contract.real_expiration_date = decode_string(&mut fields_itr)?;
      }

      wrapper.clone()
          .contract_details(req_id, contract.clone());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_contract_details_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .contract_details_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_current_time(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone()
          .current_time(decode_i64(&mut fields_itr)?);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_delta_neutral_validation(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let mut delta_neutral_contract = DeltaNeutralContract::default();

      delta_neutral_contract.con_id = decode_i32(&mut fields_itr)?;
      delta_neutral_contract.delta = decode_f64(&mut fields_itr)?;
      delta_neutral_contract.price = decode_f64(&mut fields_itr)?;

      wrapper.clone()
          .delta_neutral_validation(req_id, delta_neutral_contract);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_display_group_list(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let groups = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .display_group_list(req_id, groups.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_display_group_updated(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let contract_info = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .display_group_updated(req_id, contract_info.as_ref());
      Ok(())
  }
  fn process_error_message(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      wrapper.clone().error(
          decode_i32(&mut fields_itr)?,
          decode_i32(&mut fields_itr)?,
          decode_string(&mut fields_itr)?.as_ref(),
      );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_execution_data(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let mut version = server_version;

      if server_version < MIN_SERVER_VER_LAST_LIQUIDITY {
          version = decode_i32(&mut fields_itr)?;
      }

      let mut req_id = -1;

      if version >= 7 {
          req_id = decode_i32(&mut fields_itr)?;
      }

      let order_id = decode_i32(&mut fields_itr)?;

      // decode contract fields
      let mut contract = Contract::default();
      contract.con_id = decode_i32(&mut fields_itr)?; // ver 5 field
      contract.symbol = decode_string(&mut fields_itr)?;
      contract.sec_type = decode_string(&mut fields_itr)?;
      contract.last_trade_date_or_contract_month = decode_string(&mut fields_itr)?;
      contract.strike = decode_f64(&mut fields_itr)?;
      contract.right = decode_string(&mut fields_itr)?;
      if version >= 9 {
          contract.multiplier = decode_string(&mut fields_itr)?;
      }
      contract.exchange = decode_string(&mut fields_itr)?;
      contract.currency = decode_string(&mut fields_itr)?;
      contract.local_symbol = decode_string(&mut fields_itr)?;
      if version >= 10 {
          contract.trading_class = decode_string(&mut fields_itr)?;
      }

      // decode execution fields
      let mut execution = Execution::default();
      execution.order_id = order_id;
      execution.exec_id = decode_string(&mut fields_itr)?;
      execution.time = decode_string(&mut fields_itr)?;
      execution.acct_number = decode_string(&mut fields_itr)?;
      execution.exchange = decode_string(&mut fields_itr)?;
      execution.side = decode_string(&mut fields_itr)?;

      if server_version >= MIN_SERVER_VER_FRACTIONAL_POSITIONS {
          execution.shares = decode_f64(&mut fields_itr)?;
      } else {
          execution.shares = decode_i32(&mut fields_itr)? as f64;
      }

      execution.price = decode_f64(&mut fields_itr)?;
      execution.perm_id = decode_i32(&mut fields_itr)?; // ver 2 field
      execution.client_id = decode_i32(&mut fields_itr)?; // ver 3 field
      execution.liquidation = decode_i32(&mut fields_itr)?; // ver 4 field

      if version >= 6 {
          execution.cum_qty = decode_f64(&mut fields_itr)?;
          execution.avg_price = decode_f64(&mut fields_itr)?;
      }

      if version >= 8 {
          execution.order_ref = decode_string(&mut fields_itr)?;
      }

      if version >= 9 {
          execution.ev_rule = decode_string(&mut fields_itr)?;

          let tmp_ev_mult = (&mut fields_itr).peekable().peek().unwrap().as_str();
          if tmp_ev_mult != "" {
              execution.ev_multiplier = decode_f64(&mut fields_itr)?;
          } else {
              execution.ev_multiplier = 1.0;
          }
      }

      if server_version >= MIN_SERVER_VER_MODELS_SUPPORT {
          execution.model_code = decode_string(&mut fields_itr)?;
      }
      if server_version >= MIN_SERVER_VER_LAST_LIQUIDITY {
          execution.last_liquidity = decode_i32(&mut fields_itr)?;
      }

      wrapper.clone()
          .exec_details(req_id, contract, execution);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_execution_data_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .exec_details_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_family_codes(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let family_codes_count = decode_i32(&mut fields_itr)?;
      let mut family_codes: Vec<FamilyCode> = vec![];
      for _ in 0..family_codes_count {
          let mut fam_code = FamilyCode::default();
          fam_code.account_id = decode_string(&mut fields_itr)?;
          fam_code.family_code_str = decode_string(&mut fields_itr)?;
          family_codes.push(fam_code);
      }

      wrapper.clone()
          .family_codes(family_codes);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_fundamental_data(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let data = decode_string(&mut fields_itr)?;
      wrapper.clone()
          .fundamental_data(req_id, data.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_head_timestamp(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let timestamp = decode_string(&mut fields_itr)?;
      wrapper.clone()
          .fundamental_data(req_id, timestamp.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_histogram_data(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let num_points = decode_i32(&mut fields_itr)?;

      let mut histogram = vec![];
      for _ in 0..num_points {
          let mut data_point = HistogramData::default();
          data_point.price = decode_f64(&mut fields_itr)?;
          data_point.count = decode_i32(&mut fields_itr)?;
          histogram.push(data_point);
      }

      wrapper.clone()
          .histogram_data(req_id, histogram);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_data(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //throw away message_id
      fields_itr.next();

      if server_version < MIN_SERVER_VER_SYNT_REALTIME_BARS {
          fields_itr.next();
      }

      let req_id = decode_i32(&mut fields_itr)?;
      let start_date = decode_string(&mut fields_itr)?; // ver 2 field
      let end_date = decode_string(&mut fields_itr)?; // ver 2 field

      let _peek = *(fields_itr.clone()).peekable().peek().unwrap();

      let bar_count = decode_i32(&mut fields_itr)?;

      for _ in 0..bar_count {
          let mut bar = BarData::default();
          bar.date = decode_string(&mut fields_itr)?;
          bar.open = decode_f64(&mut fields_itr)?;
          bar.high = decode_f64(&mut fields_itr)?;
          bar.low = decode_f64(&mut fields_itr)?;
          bar.close = decode_f64(&mut fields_itr)?;
          bar.volume = if server_version < MIN_SERVER_VER_SYNT_REALTIME_BARS {
              decode_i32(&mut fields_itr)? as i64
          } else {
              decode_i64(&mut fields_itr)?
          };
          bar.average = decode_f64(&mut fields_itr)?;

          if server_version < MIN_SERVER_VER_SYNT_REALTIME_BARS {
              decode_string(&mut fields_itr)?; //has_gaps
          }

          bar.bar_count = decode_i32(&mut fields_itr)?; // ver 3 field

          wrapper.clone()
              .historical_data(req_id, bar);
      }

      // send end of dataset marker
      wrapper.clone()
          .historical_data_end(req_id, start_date.as_ref(), end_date.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_data_update(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let mut bar = BarData::default();
      bar.bar_count = decode_i32(&mut fields_itr)?;
      bar.date = decode_string(&mut fields_itr)?;
      bar.open = decode_f64(&mut fields_itr)?;
      bar.close = decode_f64(&mut fields_itr)?;
      bar.high = decode_f64(&mut fields_itr)?;
      bar.low = decode_f64(&mut fields_itr)?;
      bar.average = decode_f64(&mut fields_itr)?;
      bar.volume = decode_i64(&mut fields_itr)?;
      wrapper.clone()
          .historical_data_update(req_id, bar);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_news(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let time = decode_string(&mut fields_itr)?;
      let provider_code = decode_string(&mut fields_itr)?;
      let article_id = decode_string(&mut fields_itr)?;
      let headline = decode_string(&mut fields_itr)?;
      wrapper.clone()
          .historical_news(
              req_id,
              time.as_ref(),
              provider_code.as_ref(),
              article_id.as_ref(),
              headline.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_news_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let has_more = decode_bool(&mut fields_itr)?;

      wrapper.clone()
          .historical_news_end(req_id, has_more);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_ticks(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let tick_count = decode_i32(&mut fields_itr)?;

      let mut ticks = vec![];

      for _ in 0..tick_count {
          let mut historical_tick = HistoricalTick::default();
          historical_tick.time = decode_i32(&mut fields_itr)?;
          fields_itr.next(); // for consistency
          historical_tick.price = decode_f64(&mut fields_itr)?;
          historical_tick.size = decode_i32(&mut fields_itr)?;
          ticks.push(historical_tick);
      }

      let done = decode_bool(&mut fields_itr)?;

      wrapper.clone()
          .historical_ticks(req_id, ticks, done);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_ticks_bid_ask(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let tick_count = decode_i32(&mut fields_itr)?;

      let mut ticks = vec![];

      for _ in 0..tick_count {
          let mut historical_tick_bid_ask = HistoricalTickBidAsk::default();
          historical_tick_bid_ask.time = decode_i32(&mut fields_itr)?;
          let mask = decode_i32(&mut fields_itr)?;
          let mut tick_attrib_bid_ask = TickAttribBidAsk::default();
          tick_attrib_bid_ask.ask_past_high = mask & 1 != 0;
          tick_attrib_bid_ask.bid_past_low = mask & 2 != 0;
          historical_tick_bid_ask.tick_attrib_bid_ask = tick_attrib_bid_ask;
          historical_tick_bid_ask.price_bid = decode_f64(&mut fields_itr)?;
          historical_tick_bid_ask.price_ask = decode_f64(&mut fields_itr)?;
          historical_tick_bid_ask.size_bid = decode_i32(&mut fields_itr)?;
          historical_tick_bid_ask.size_ask = decode_i32(&mut fields_itr)?;
          ticks.push(historical_tick_bid_ask);
      }

      let done = decode_bool(&mut fields_itr)?;

      wrapper.clone()
          .historical_ticks_bid_ask(req_id, ticks, done);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_historical_ticks_last(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let tick_count = decode_i32(&mut fields_itr)?;

      let mut ticks = vec![];

      for _ in 0..tick_count {
          let mut historical_tick_last = HistoricalTickLast::default();
          historical_tick_last.time = decode_i32(&mut fields_itr)?;
          let mask = decode_i32(&mut fields_itr)?;
          let mut tick_attrib_last = TickAttribLast::default();
          tick_attrib_last.past_limit = mask & 1 != 0;
          tick_attrib_last.unreported = mask & 2 != 0;
          historical_tick_last.tick_attrib_last = tick_attrib_last;
          historical_tick_last.price = decode_f64(&mut fields_itr)?;
          historical_tick_last.size = decode_i32(&mut fields_itr)?;
          historical_tick_last.exchange = decode_string(&mut fields_itr)?;
          historical_tick_last.special_conditions = decode_string(&mut fields_itr)?;
          ticks.push(historical_tick_last);
      }

      let done = decode_bool(&mut fields_itr)?;

      wrapper.clone()
          .historical_ticks_last(req_id, ticks, done);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_managed_accounts(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let accounts_list = decode_string(&mut fields_itr)?;
      info!("calling managed_accounts");
      wrapper.clone()
          .managed_accounts(accounts_list.as_ref());
      info!("finished calling managed_accounts");
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_market_data_type(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();
      let req_id = decode_i32(&mut fields_itr)?;
      let market_data_type = decode_i32(&mut fields_itr)?;
      wrapper.clone()
          .market_data_type(req_id, market_data_type);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_market_depth(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let position = decode_i32(&mut fields_itr)?;
      let operation = decode_i32(&mut fields_itr)?;
      let side = decode_i32(&mut fields_itr)?;
      let price = decode_f64(&mut fields_itr)?;
      let size = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .update_mkt_depth(req_id, position, operation, side, price, size);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_market_depth_l2(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let position = decode_i32(&mut fields_itr)?;
      let market_maker = decode_string(&mut fields_itr)?;
      let operation = decode_i32(&mut fields_itr)?;
      let side = decode_i32(&mut fields_itr)?;
      let price = decode_f64(&mut fields_itr)?;
      let size = decode_i32(&mut fields_itr)?;
      let mut is_smart_depth = false;

      if server_version >= MIN_SERVER_VER_SMART_DEPTH {
          is_smart_depth = decode_bool(&mut fields_itr)?;
      }

      wrapper.clone()
          .update_mkt_depth_l2(
              req_id,
              position,
              market_maker.as_ref(),
              operation,
              side,
              price,
              size,
              is_smart_depth,
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_market_rule(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let market_rule_id = decode_i32(&mut fields_itr)?;

      let price_increments_count = decode_i32(&mut fields_itr)?;
      let mut price_increments = vec![];

      for _ in 0..price_increments_count {
          let mut prc_inc = PriceIncrement::default();
          prc_inc.low_edge = decode_f64(&mut fields_itr)?;
          prc_inc.increment = decode_f64(&mut fields_itr)?;
          price_increments.push(prc_inc);
      }

      wrapper.clone()
          .market_rule(market_rule_id, price_increments);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_market_depth_exchanges(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let mut depth_mkt_data_descriptions = vec![];
      let depth_mkt_data_descriptions_count = decode_i32(&mut fields_itr)?;

      for _ in 0..depth_mkt_data_descriptions_count {
          let mut desc = DepthMktDataDescription::default();
          desc.exchange = decode_string(&mut fields_itr)?;
          desc.sec_type = decode_string(&mut fields_itr)?;
          if server_version >= MIN_SERVER_VER_SERVICE_DATA_TYPE {
              desc.listing_exch = decode_string(&mut fields_itr)?;
              desc.service_data_type = decode_string(&mut fields_itr)?;
              desc.agg_group = decode_i32(&mut fields_itr)?;
          } else {
              decode_i32(&mut fields_itr)?; // boolean notSuppIsL2
          }
          depth_mkt_data_descriptions.push(desc);
      }

      wrapper.clone()
          .mkt_depth_exchanges(depth_mkt_data_descriptions);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_news_article(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let article_type = decode_i32(&mut fields_itr)?;
      let article_text = decode_string(&mut fields_itr)?;
      wrapper.clone()
          .news_article(req_id, article_type, article_text.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_news_bulletins(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let news_msg_id = decode_i32(&mut fields_itr)?;
      let news_msg_type = decode_i32(&mut fields_itr)?;
      let news_message = decode_string(&mut fields_itr)?;
      let originating_exch = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .update_news_bulletin(
              news_msg_id,
              news_msg_type,
              news_message.as_ref(),
              originating_exch.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_news_providers(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let mut news_providers = vec![];
      let news_providers_count = decode_i32(&mut fields_itr)?;
      for _ in 0..news_providers_count {
          let mut provider = NewsProvider::default();
          provider.code = decode_string(&mut fields_itr)?;
          provider.name = decode_string(&mut fields_itr)?;
          news_providers.push(provider);
      }

      wrapper.clone()
          .news_providers(news_providers);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_next_valid_id(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let order_id = decode_i32(&mut fields_itr)?;
      wrapper.clone()
          .next_valid_id(order_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_open_order(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //info!("Processing open order");
      //throw away message_id
      fields_itr.next();

      let mut order = Order::default();
      let mut contract = Contract::default();
      let mut order_state = OrderState::default();

      let mut version = server_version;
      if server_version < MIN_SERVER_VER_ORDER_CONTAINER {
          version = decode_i32(&mut fields_itr)?;
      }

      let mut order_decoder = OrderDecoder::new(
          &mut contract,
          &mut order,
          &mut order_state,
          version,
          server_version,
      );

      order_decoder.decode_open(&mut fields_itr)?;

      wrapper.clone()
          .open_order(order.order_id, contract, order, order_state);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_open_order_end(wrapper: &Arc<T>,_fields: &[String]) -> Result<(), IBKRApiLibError> {
      wrapper.clone()
          .open_order_end();
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_order_bound(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let api_client_id = decode_i32(&mut fields_itr)?;
      let api_order_id = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .order_bound(req_id, api_client_id, api_order_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_order_status(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      if server_version < MIN_SERVER_VER_MARKET_CAP_PRICE {
          fields_itr.next();
      }

      let order_id = decode_i32(&mut fields_itr)?;

      let status = decode_string(&mut fields_itr)?;

      let filled;
      if server_version >= MIN_SERVER_VER_FRACTIONAL_POSITIONS {
          filled = decode_f64(&mut fields_itr)?;
      } else {
          filled = decode_i32(&mut fields_itr)? as f64;
      }

      let remaining;

      if server_version >= MIN_SERVER_VER_FRACTIONAL_POSITIONS {
          remaining = decode_f64(&mut fields_itr)?;
      } else {
          remaining = decode_i32(&mut fields_itr)? as f64;
      }

      let avg_fill_price = decode_f64(&mut fields_itr)?;

      let perm_id = decode_i32(&mut fields_itr)?; // ver 2 field
      let parent_id = decode_i32(&mut fields_itr)?; // ver 3 field
      let last_fill_price = decode_f64(&mut fields_itr)?; // ver 4 field
      let client_id = decode_i32(&mut fields_itr)?; // ver 5 field
      let why_held = decode_string(&mut fields_itr)?; // ver 6 field

      let mut mkt_cap_price = 0.0;
      if server_version >= MIN_SERVER_VER_MARKET_CAP_PRICE {
          mkt_cap_price = decode_f64(&mut fields_itr)?;
      }

      wrapper.clone()
          .order_status(
              order_id,
              status.as_ref(),
              filled,
              remaining,
              avg_fill_price,
              perm_id,
              parent_id,
              last_fill_price,
              client_id,
              why_held.as_ref(),
              mkt_cap_price,
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_pnl(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let daily_pnl = decode_f64(&mut fields_itr)?;
      let mut unrealized_pnl = 0.0;
      let mut realized_pnl = 0.0;

      if server_version >= MIN_SERVER_VER_UNREALIZED_PNL {
          unrealized_pnl = decode_f64(&mut fields_itr)?;
      }

      if server_version >= MIN_SERVER_VER_REALIZED_PNL {
          realized_pnl = decode_f64(&mut fields_itr)?;
      }

      wrapper.clone().pnl(
          req_id,
          daily_pnl,
          unrealized_pnl,
          realized_pnl,
      );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_pnl_single(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let pos = decode_i32(&mut fields_itr)?;
      let daily_pnl = decode_f64(&mut fields_itr)?;
      let mut unrealized_pnl = 0.0;
      let mut realized_pnl = 0.0;

      if server_version >= MIN_SERVER_VER_UNREALIZED_PNL {
          unrealized_pnl = decode_f64(&mut fields_itr)?;
      }

      if server_version >= MIN_SERVER_VER_REALIZED_PNL {
          realized_pnl = decode_f64(&mut fields_itr)?;
      }

      let value = decode_f64(&mut fields_itr)?;

      wrapper.clone()
          .pnl_single(req_id, pos, daily_pnl, unrealized_pnl, realized_pnl, value);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_portfolio_value(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let version = decode_i32(&mut fields_itr)?;

      // read contract fields
      let mut contract = Contract::default();
      contract.con_id = decode_i32(&mut fields_itr)?; // ver 6 field
      contract.symbol = decode_string(&mut fields_itr)?;
      contract.sec_type = decode_string(&mut fields_itr)?;
      contract.last_trade_date_or_contract_month = decode_string(&mut fields_itr)?;
      contract.strike = decode_f64(&mut fields_itr)?;
      contract.right = decode_string(&mut fields_itr)?;

      if version >= 7 {
          contract.multiplier = decode_string(&mut fields_itr)?;
          contract.primary_exchange = decode_string(&mut fields_itr)?;
      }

      contract.currency = decode_string(&mut fields_itr)?;
      contract.local_symbol = decode_string(&mut fields_itr)?; // ver 2 field
      if version >= 8 {
          contract.trading_class = decode_string(&mut fields_itr)?;
      }

      let position;
      if server_version >= MIN_SERVER_VER_FRACTIONAL_POSITIONS {
          position = decode_f64(&mut fields_itr)?;
      } else {
          position = decode_i32(&mut fields_itr)? as f64;
      }

      let market_price = decode_f64(&mut fields_itr)?;
      let market_value = decode_f64(&mut fields_itr)?;
      let average_cost = decode_f64(&mut fields_itr)?; // ver 3 field
      let unrealized_pnl = decode_f64(&mut fields_itr)?; // ver 3 field
      let realized_pnl = decode_f64(&mut fields_itr)?; // ver 3 field

      let account_name = decode_string(&mut fields_itr)?; // ver 4 field

      if version == 6 && server_version == 39 {
          contract.primary_exchange = decode_string(&mut fields_itr)?;
      }

      wrapper.clone()
          .update_portfolio(
              contract,
              position,
              market_price,
              market_value,
              average_cost,
              unrealized_pnl,
              realized_pnl,
              account_name.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_position_data(wrapper: &Arc<T>, server_version: i32, fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let version = decode_i32(&mut fields_itr)?;

      let account = decode_string(&mut fields_itr)?;

      // decode contract fields
      let mut contract = Contract::default();
      contract.con_id = decode_i32(&mut fields_itr)?;
      contract.symbol = decode_string(&mut fields_itr)?;
      contract.sec_type = decode_string(&mut fields_itr)?;
      contract.last_trade_date_or_contract_month = decode_string(&mut fields_itr)?;
      contract.strike = decode_f64(&mut fields_itr)?;
      contract.right = decode_string(&mut fields_itr)?;
      contract.multiplier = decode_string(&mut fields_itr)?;
      contract.exchange = decode_string(&mut fields_itr)?;
      contract.currency = decode_string(&mut fields_itr)?;
      contract.local_symbol = decode_string(&mut fields_itr)?;
      if version >= 2 {
          contract.trading_class = decode_string(&mut fields_itr)?;
      }

      let position;
      if server_version >= MIN_SERVER_VER_FRACTIONAL_POSITIONS {
          position = decode_f64(&mut fields_itr)?;
      } else {
          position = decode_i32(&mut fields_itr)? as f64;
      }

      let mut avg_cost = 0.0;
      if version >= 3 {
          avg_cost = decode_f64(&mut fields_itr)?;
      }

      wrapper.clone().position(
          account.as_ref(),
          contract,
          position,
          avg_cost,
      );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_position_end(wrapper: &Arc<T>,_fields: &[String]) -> Result<(), IBKRApiLibError> {
      wrapper.clone()
          .position_end();
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_position_multi(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let account = decode_string(&mut fields_itr)?;

      // decode contract fields
      let mut contract = Contract::default();
      contract.con_id = decode_i32(&mut fields_itr)?;
      contract.symbol = decode_string(&mut fields_itr)?;
      contract.sec_type = decode_string(&mut fields_itr)?;
      contract.last_trade_date_or_contract_month = decode_string(&mut fields_itr)?;
      contract.strike = decode_f64(&mut fields_itr)?;
      contract.right = decode_string(&mut fields_itr)?;
      contract.multiplier = decode_string(&mut fields_itr)?;
      contract.exchange = decode_string(&mut fields_itr)?;
      contract.currency = decode_string(&mut fields_itr)?;
      contract.local_symbol = decode_string(&mut fields_itr)?;
      contract.trading_class = decode_string(&mut fields_itr)?;

      let position = decode_f64(&mut fields_itr)?;
      let avg_cost = decode_f64(&mut fields_itr)?;
      let model_code = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .position_multi(
              req_id,
              account.as_ref(),
              model_code.as_ref(),
              contract,
              position,
              avg_cost,
          );

      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_position_multi_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      wrapper.clone()
          .position_multi_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_real_time_bars(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let mut bar = RealTimeBar::default();
      bar.date_time = decode_string(&mut fields_itr)?;
      bar.open = decode_f64(&mut fields_itr)?;
      bar.high = decode_f64(&mut fields_itr)?;
      bar.low = decode_f64(&mut fields_itr)?;
      bar.close = decode_f64(&mut fields_itr)?;
      bar.volume = decode_i64(&mut fields_itr)?;
      bar.wap = decode_f64(&mut fields_itr)?;
      bar.count = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .realtime_bar(req_id, bar);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_receive_fa(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let fa_data_type = decode_i32(&mut fields_itr)?;
      let xml = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .receive_fa(FromPrimitive::from_i32(fa_data_type).unwrap(), xml.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_reroute_mkt_data_req(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let con_id = decode_i32(&mut fields_itr)?;
      let exchange = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .reroute_mkt_data_req(req_id, con_id, exchange.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_reroute_mkt_depth_req(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      let con_id = decode_i32(&mut fields_itr)?;
      let exchange = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .reroute_mkt_depth_req(req_id, con_id, exchange.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_scanner_data(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let number_of_elements = decode_i32(&mut fields_itr)?;

      for _ in 0..number_of_elements {
          let mut data = ScanData::default();
          data.contract = ContractDetails::default();

          data.rank = decode_i32(&mut fields_itr)?;
          data.contract.contract.con_id = decode_i32(&mut fields_itr)?; // ver 3 field
          data.contract.contract.symbol = decode_string(&mut fields_itr)?;
          data.contract.contract.sec_type = decode_string(&mut fields_itr)?;
          data.contract.contract.last_trade_date_or_contract_month =
              decode_string(&mut fields_itr)?;
          data.contract.contract.strike = decode_f64(&mut fields_itr)?;
          data.contract.contract.right = decode_string(&mut fields_itr)?;
          data.contract.contract.exchange = decode_string(&mut fields_itr)?;
          data.contract.contract.currency = decode_string(&mut fields_itr)?;
          data.contract.contract.local_symbol = decode_string(&mut fields_itr)?;
          data.contract.market_name = decode_string(&mut fields_itr)?;
          data.contract.contract.trading_class = decode_string(&mut fields_itr)?;
          data.distance = decode_string(&mut fields_itr)?;
          data.benchmark = decode_string(&mut fields_itr)?;
          data.projection = decode_string(&mut fields_itr)?;
          data.legs = decode_string(&mut fields_itr)?;
          wrapper.clone()
              .scanner_data(
                  req_id,
                  data.rank,
                  data.contract,
                  data.distance.as_ref(),
                  data.benchmark.as_ref(),
                  data.projection.as_ref(),
                  data.legs.as_ref(),
              );
      }

      wrapper.clone()
          .scanner_data_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_scanner_parameters(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let xml = decode_string(&mut fields_itr)?;
      wrapper.clone()
          .scanner_parameters(xml.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_security_definition_option_parameter(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let exchange = decode_string(&mut fields_itr)?;
      let underlying_con_id = decode_i32(&mut fields_itr)?;
      let trading_class = decode_string(&mut fields_itr)?;
      let multiplier = decode_string(&mut fields_itr)?;

      let exp_count = decode_i32(&mut fields_itr)?;
      let mut expirations = HashSet::new();
      for _ in 0..exp_count {
          let expiration = decode_string(&mut fields_itr)?;
          expirations.insert(expiration);
      }

      let strike_count = decode_i32(&mut fields_itr)?;
      let mut strikes = HashSet::new();
      for _ in 0..strike_count {
          let strike = decode_f64(&mut fields_itr)?;
          let big_strike = BigDecimal::from_f64(strike).unwrap();
          strikes.insert(big_strike);
      }

      wrapper.clone()
          .security_definition_option_parameter(
              req_id,
              exchange.as_ref(),
              underlying_con_id,
              trading_class.as_ref(),
              multiplier.as_ref(),
              expirations,
              strikes,
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_security_definition_option_parameter_end(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;
      wrapper.clone()
          .security_definition_option_parameter_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_smart_components(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let count = decode_i32(&mut fields_itr)?;

      let mut smart_components = vec![];
      for _ in 0..count {
          let mut smart_component = SmartComponent::default();
          smart_component.bit_number = decode_i32(&mut fields_itr)?;
          smart_component.exchange = decode_string(&mut fields_itr)?;
          smart_component.exchange_letter = decode_string(&mut fields_itr)?;
          smart_components.push(smart_component)
      }

      wrapper.clone()
          .smart_components(req_id, smart_components);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_soft_dollar_tiers(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let count = decode_i32(&mut fields_itr)?;

      let mut tiers = vec![];
      for _ in 0..count {
          let mut tier = SoftDollarTier::default();
          tier.name = decode_string(&mut fields_itr)?;
          tier.val = decode_string(&mut fields_itr)?;
          tier.display_name = decode_string(&mut fields_itr)?;
          tiers.push(tier);
      }

      wrapper.clone()
          .soft_dollar_tiers(req_id, tiers);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_symbol_samples(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let count = decode_i32(&mut fields_itr)?;
      let mut contract_descriptions = vec![];
      for _ in 0..count {
          let mut con_desc = ContractDescription::default();
          con_desc.contract.con_id = decode_i32(&mut fields_itr)?;
          con_desc.contract.symbol = decode_string(&mut fields_itr)?;
          con_desc.contract.sec_type = decode_string(&mut fields_itr)?;
          con_desc.contract.primary_exchange = decode_string(&mut fields_itr)?;
          con_desc.contract.currency = decode_string(&mut fields_itr)?;

          let derivative_sec_types_cnt = decode_i32(&mut fields_itr)?;
          con_desc.derivative_sec_types = vec![];
          for _ in 0..derivative_sec_types_cnt {
              let deriv_sec_type = decode_string(&mut fields_itr)?;
              con_desc.derivative_sec_types.push(deriv_sec_type);
          }
          contract_descriptions.push(con_desc)
      }
      wrapper.clone()
          .symbol_samples(req_id, contract_descriptions);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_by_tick(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      let tick_type = decode_i32(&mut fields_itr)?;
      let time = decode_i64(&mut fields_itr)?;

      match tick_type {
          0 => return Ok(()), // None
          1..=2 =>
          // Last (1) or AllLast (2)
          {
              let price = decode_f64(&mut fields_itr)?;
              let size = decode_i32(&mut fields_itr)?;
              let mask = decode_i32(&mut fields_itr)?;
              let mut tick_attrib_last = TickAttribLast::default();
              tick_attrib_last.past_limit = mask & 1 != 0;
              tick_attrib_last.unreported = mask & 2 != 0;
              let exchange = decode_string(&mut fields_itr)?;
              let special_conditions = decode_string(&mut fields_itr)?;
              wrapper.clone()
                  .tick_by_tick_all_last(
                      req_id,
                      FromPrimitive::from_i32(tick_type).unwrap(),
                      time,
                      price,
                      size,
                      tick_attrib_last,
                      exchange.as_ref(),
                      special_conditions.as_ref(),
                  );
          }
          3 =>
          // BidAsk
          {
              let bid_price = decode_f64(&mut fields_itr)?;
              let ask_price = decode_f64(&mut fields_itr)?;
              let bid_size = decode_i32(&mut fields_itr)?;
              let ask_size = decode_i32(&mut fields_itr)?;
              let mask = decode_i32(&mut fields_itr)?;
              let mut tick_attrib_bid_ask = TickAttribBidAsk::default();
              tick_attrib_bid_ask.bid_past_low = mask & 1 != 0;
              tick_attrib_bid_ask.ask_past_high = mask & 2 != 0;
              wrapper.clone()
                  .tick_by_tick_bid_ask(
                      req_id,
                      time,
                      bid_price,
                      ask_price,
                      bid_size,
                      ask_size,
                      tick_attrib_bid_ask,
                  );
          }
          4 =>
          // MidPoint
          {
              let mid_point = decode_f64(&mut fields_itr)?;
              wrapper.clone()
                  .tick_by_tick_mid_point(req_id, time, mid_point);
          }
          _ => return Ok(()),
      }
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  #[allow(dead_code)]
  fn process_tick_efp(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let ticker_id = decode_i32(&mut fields_itr)?;
      let tick_type = decode_i32(&mut fields_itr)?;
      let basis_points = decode_f64(&mut fields_itr)?;
      let formatted_basis_points = decode_string(&mut fields_itr)?;
      let implied_futures_price = decode_f64(&mut fields_itr)?;
      let hold_days = decode_i32(&mut fields_itr)?;
      let future_last_trade_date = decode_string(&mut fields_itr)?;
      let dividend_impact = decode_f64(&mut fields_itr)?;
      let dividends_to_last_trade_date = decode_f64(&mut fields_itr)?;
      wrapper.clone().tick_efp(
          ticker_id,
          FromPrimitive::from_i32(tick_type).unwrap(),
          basis_points,
          formatted_basis_points.as_ref(),
          implied_futures_price,
          hold_days,
          future_last_trade_date.as_ref(),
          dividend_impact,
          dividends_to_last_trade_date,
      );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_generic(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let ticker_id = decode_i32(&mut fields_itr)?;
      let tick_type = decode_i32(&mut fields_itr)?;
      let value = decode_f64(&mut fields_itr)?;

      wrapper.clone()
          .tick_generic(
              ticker_id,
              FromPrimitive::from_i32(tick_type).unwrap(),
              value,
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_news(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      let ticker_id = decode_i32(&mut fields_itr)?;
      let time_stamp = decode_i32(&mut fields_itr)?;
      let provider_code = decode_string(&mut fields_itr)?;
      let article_id = decode_string(&mut fields_itr)?;
      let headline = decode_string(&mut fields_itr)?;
      let extra_data = decode_string(&mut fields_itr)?;
      wrapper.clone()
          .tick_news(
              ticker_id,
              time_stamp,
              provider_code.as_ref(),
              article_id.as_ref(),
              headline.as_ref(),
              extra_data.as_ref(),
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_option_computation(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let version = decode_i32(&mut fields_itr)?;
      let ticker_id = decode_i32(&mut fields_itr)?;
      let tick_type = decode_i32(&mut fields_itr)?;
      let mut implied_vol = decode_f64(&mut fields_itr)?;
      if approx_eq!(f64, implied_vol, -1.0, ulps = 2) {
          // -1 is the "not yet computed" indicator
          implied_vol = f64::max_value();
      }

      let mut delta = decode_f64(&mut fields_itr)?;
      if approx_eq!(f64, delta, -2.0, ulps = 2) {
          // -2 is the "not yet computed" indicator
          delta = f64::max_value();
      }
      let mut opt_price = f64::max_value();
      let mut pv_dividend = f64::max_value();
      let mut gamma = f64::max_value();
      let mut vega = f64::max_value();
      let mut theta = f64::max_value();
      let mut und_price = f64::max_value();
      if version >= 6
          || tick_type == TickType::ModelOption as i32
          || tick_type == TickType::DelayedModelOption as i32
      {
          // introduced in version == 5
          opt_price = decode_f64(&mut fields_itr)?;
          if approx_eq!(f64, opt_price, -1.0, ulps = 2) {
              // -1 is the "not yet computed" indicator
              opt_price = f64::max_value();
          }
          pv_dividend = decode_f64(&mut fields_itr)?;
          if approx_eq!(f64, pv_dividend, -1.0, ulps = 2) {
              // -1 is the "not yet computed" indicator
              pv_dividend = f64::max_value();
          }
      }
      if version >= 6 {
          gamma = decode_f64(&mut fields_itr)?;
          if approx_eq!(f64, gamma, -2.0, ulps = 2) {
              // -2 is the "not yet computed" indicator
              gamma = f64::max_value();
          }
          vega = decode_f64(&mut fields_itr)?;
          if approx_eq!(f64, vega, -2.0, ulps = 2) {
              // -2 is the "not yet computed" indicator
              vega = f64::max_value();
          }
          theta = decode_f64(&mut fields_itr)?;
          if approx_eq!(f64, theta, -2.0, ulps = 2) {
              // -2 is the "not yet computed" indicator
              theta = f64::max_value();
          }
          und_price = decode_f64(&mut fields_itr)?;
          if approx_eq!(f64, und_price, -1.0, ulps = 2) {
              // -1 is the "not yet computed" indicator
              und_price = f64::max_value();
          }
      }

      wrapper.clone()
          .tick_option_computation(
              ticker_id,
              FromPrimitive::from_i32(tick_type).unwrap(),
              implied_vol,
              delta,
              opt_price,
              pv_dividend,
              gamma,
              vega,
              theta,
              und_price,
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_req_params(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();

      let ticker_id = decode_i32(&mut fields_itr)?;
      let min_tick = decode_f64(&mut fields_itr)?;
      let bbo_exchange = decode_string(&mut fields_itr)?;
      let snapshot_permissions = decode_i32(&mut fields_itr)?;
      wrapper.clone()
          .tick_req_params(
              ticker_id,
              min_tick,
              bbo_exchange.as_ref(),
              snapshot_permissions,
          );
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_size(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let ticker_id = decode_i32(&mut fields_itr)?;
      let tick_type = decode_i32(&mut fields_itr)?;
      let size = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .tick_size(ticker_id, FromPrimitive::from_i32(tick_type).unwrap(), size);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_tick_snapshot_end(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let req_id = decode_i32(&mut fields_itr)?;

      wrapper.clone()
          .tick_snapshot_end(req_id);
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_verify_and_auth_completed(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();
      let _is_successful_str = decode_string(&mut fields_itr)?;
      let is_successful = "true" == decode_string(&mut fields_itr)?;
      let error_text = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .verify_and_auth_completed(is_successful, error_text.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_verify_and_auth_message_api(
      wrapper: &Arc<T>,
      fields: &[String],
  ) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();

      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let api_data = decode_string(&mut fields_itr)?;
      let xyz_challenge = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .verify_and_auth_message_api(api_data.as_ref(), xyz_challenge.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn process_verify_completed(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let _is_successful_str = decode_string(&mut fields_itr)?;
      let is_successful = "true" == decode_string(&mut fields_itr)?;
      let error_text = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .verify_completed(is_successful, error_text.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  #[allow(dead_code)]
  fn process_verify_message_api(wrapper: &Arc<T>,fields: &[String]) -> Result<(), IBKRApiLibError> {
      let mut fields_itr = fields.iter();
      //throw away message_id
      fields_itr.next();
      //throw away version
      fields_itr.next();

      let api_data = decode_string(&mut fields_itr)?;

      wrapper.clone()
          .verify_message_api(api_data.as_ref());
      Ok(())
  }

  //----------------------------------------------------------------------------------------------
  fn read_last_trade_date(
      wrapper: &Arc<T>,
      contract: &mut ContractDetails,
      is_bond: bool,
      read_date: &str,
  ) -> Result<(), IBKRApiLibError> {
      if read_date != "" {
          let splitted = read_date.split_whitespace().collect::<Vec<&str>>();
          if splitted.len() > 0 {
              if is_bond {
                  contract.maturity = splitted.get(0).unwrap_or_else(|| &"").to_string();
              } else {
                  contract.contract.last_trade_date_or_contract_month =
                      splitted.get(0).unwrap_or_else(|| &"").to_string();
              }
          }
          if splitted.len() > 1 {
              contract.last_trade_time = splitted.get(1).unwrap_or_else(|| &"").to_string();
          }
          if is_bond && splitted.len() > 2 {
              contract.time_zone_id = splitted.get(2).unwrap_or_else(|| &"").to_string();
          }
      }
      Ok(())
  }
}